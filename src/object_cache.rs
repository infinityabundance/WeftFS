//! A bounded cache of verified **decoded** objects.
//!
//! The artifact cached here is the expensive one: the exact decoded bytes of
//! one object version, after every chunk/frame hash, dependency hash, logical
//! root, and whole-content hash has already been verified. A cache hit returns
//! bytes that are byte-identical to what a fresh reconstruction would produce,
//! so no reader ever observes unverified or approximate content.
//!
//! ## Identity
//!
//! The key is `(object id, content identity)`. `content` is the manifest id the
//! object record names, which changes with every published version, so an entry
//! can never satisfy a read for a different version. Readers that only hold a
//! content root (a dependency inside a delta frame, or the FUSE `read_content`
//! path) use the content root as the object part of the key; the two forms are
//! distinct identities and are never confused.
//!
//! ## Admission and eviction
//!
//! Every entry carries the score
//!
//! ```text
//! expected_cost_avoided_ns = decode_cost_ns(class, decoded_bytes)
//!                          * dependent_count
//!                          * observed_read_frequency
//! ```
//!
//! where `decode_cost_ns` comes from the existing measured
//! [`crate::cost_model`] table, `dependent_count` is the number of `derived_from`
//! relation-graph edges that target the object, and
//! `observed_read_frequency` is the existing advisory access counter. The
//! eviction order is:
//!
//! 1. lower `expected_cost_avoided_ns / decoded_bytes` first (cost avoided per
//!    cached byte);
//! 2. then lower `expected_cost_avoided_ns`;
//! 3. then **larger** decoded size (free the most memory for the same score);
//! 4. then lower content identity, then lower object id.
//!
//! Comparison is exact integer arithmetic (cross-multiplied `u128`), so the
//! order is deterministic and reproducible.
//!
//! A shared base with many dependents therefore outranks a one-off large
//! decoded tensor. An object explicitly pinned with
//! [`ObjectCache::pin`] is never evicted; if pinned entries alone fill the
//! budget, admission of an unpinned entry is simply refused (the cache stays
//! within its bound, and the read still returns correct bytes).
//!
//! ## Authorization
//!
//! The cache is never an authorization oracle. `Store::authorize_read` runs
//! before every lookup, so a caller without read authority is denied even when
//! the bytes are cached, and a denial never reveals whether an entry exists.
//!
//! The cache is per open store handle, in memory only, absent from every frozen
//! format, and never consulted by commits, verification, or GC.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::cost_model::{self, CostClass, CostModelKind};
use crate::error::{Error, Result};
use crate::identity::Hash32;

/// Hard bound on retained entries, independent of the byte budget. Keeps the
/// bookkeeping (and the eviction scan) bounded even for tiny objects.
pub const MAX_CACHE_ENTRIES: usize = 65_536;

/// Hard bound on explicit pins.
pub const MAX_CACHE_PINS: usize = 65_536;

/// How many pins a store may hold; the cache refuses more with
/// `LimitExceeded`.
pub const DEFAULT_MAX_PINS: usize = 4_096;

/// Configuration for [`ObjectCache`]. Disabled by default (`max_bytes == 0`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectCacheOptions {
    pub max_bytes: u64,
    pub max_entries: usize,
    pub max_pins: usize,
    pub cost_model: CostModelKind,
}

impl Default for ObjectCacheOptions {
    fn default() -> Self {
        ObjectCacheOptions {
            max_bytes: 0,
            max_entries: MAX_CACHE_ENTRIES,
            max_pins: DEFAULT_MAX_PINS,
            cost_model: CostModelKind::default(),
        }
    }
}

impl ObjectCacheOptions {
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn enabled(&self) -> bool {
        self.max_bytes > 0
    }

    pub fn validate(&self) -> Result<()> {
        if self.max_entries == 0 {
            return Err(Error::invalid_argument(
                "object cache max_entries must be at least 1",
            ));
        }
        if self.max_pins == 0 {
            return Err(Error::invalid_argument(
                "object cache max_pins must be at least 1",
            ));
        }
        Ok(())
    }
}

/// The identity of one cached decoded object version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CacheKey {
    pub object: Hash32,
    /// Manifest / representation identity. A new version has a new value.
    pub content: Hash32,
}

impl CacheKey {
    pub fn new(object: Hash32, content: Hash32) -> Self {
        CacheKey { object, content }
    }
}

/// The observed inputs to the admission score.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheScore {
    /// `decode_cost(class, decoded_bytes)` from the measured cost table.
    pub decode_ns: u64,
    /// Number of relation-graph `derived_from` edges that target the object.
    pub dependents: u64,
    /// Advisory observed read count (at least 1 for the read being admitted).
    pub reads: u64,
}

impl CacheScore {
    /// Expected reconstruction time avoided by one hit, saturating.
    pub fn expected_ns(&self) -> u128 {
        (self.decode_ns as u128)
            .saturating_mul(self.dependents as u128)
            .saturating_mul(self.reads as u128)
    }
}

struct Entry {
    bytes: Arc<Vec<u8>>,
    score: CacheScore,
}

/// What the cache has done, for tests and receipts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ObjectCacheStats {
    pub enabled: bool,
    pub max_bytes: u64,
    pub max_entries: usize,
    pub entries: usize,
    pub bytes: u64,
    pub pinned_objects: usize,
    pub hits: u64,
    pub misses: u64,
    pub admissions: u64,
    /// Entries removed to make room or to make the bytes bound hold.
    pub evictions: u64,
    /// Entries removed because a newer version of the same object was admitted.
    pub stale_removals: u64,
    /// Successful lookups' returned bytes.
    pub bytes_served: u64,
    /// Admissions refused because the bytes would not fit or the entry bound
    /// was full of pinned entries.
    pub rejections: u64,
    /// Evictions that had to skip a pinned entry.
    pub pinned_skips: u64,
}

/// A bounded, deterministic cache of verified decoded object bytes.
pub struct ObjectCache {
    options: ObjectCacheOptions,
    entries: BTreeMap<CacheKey, Entry>,
    pins: BTreeSet<Hash32>,
    bytes: u64,
    hits: u64,
    misses: u64,
    admissions: u64,
    evictions: u64,
    stale_removals: u64,
    bytes_served: u64,
    rejections: u64,
    pinned_skips: u64,
}

impl ObjectCache {
    pub fn new(options: ObjectCacheOptions) -> Self {
        let options = if options.validate().is_ok() {
            options
        } else {
            ObjectCacheOptions::disabled()
        };
        let mut cache = ObjectCache {
            options,
            entries: BTreeMap::new(),
            pins: BTreeSet::new(),
            bytes: 0,
            hits: 0,
            misses: 0,
            admissions: 0,
            evictions: 0,
            stale_removals: 0,
            bytes_served: 0,
            rejections: 0,
            pinned_skips: 0,
        };
        // A disabled cache has no capacity; enabling later must also shrink.
        cache.fit_to_bound();
        cache
    }

    /// Replace the configuration. Call only while the store is not being read.
    pub fn set_options(&mut self, options: ObjectCacheOptions) {
        if options.validate().is_err() {
            return;
        }
        self.options = options;
        self.fit_to_bound();
    }

    pub fn options(&self) -> ObjectCacheOptions {
        self.options
    }

    pub fn enabled(&self) -> bool {
        self.options.enabled()
    }

    pub fn stats(&self) -> ObjectCacheStats {
        ObjectCacheStats {
            enabled: self.enabled(),
            max_bytes: self.options.max_bytes,
            max_entries: self.options.max_entries,
            entries: self.entries.len(),
            bytes: self.bytes,
            pinned_objects: self.pins.len(),
            hits: self.hits,
            misses: self.misses,
            admissions: self.admissions,
            evictions: self.evictions,
            stale_removals: self.stale_removals,
            bytes_served: self.bytes_served,
            rejections: self.rejections,
            pinned_skips: self.pinned_skips,
        }
    }

    /// True when `object` is pinned. Pins outlive entries: pinning before the
    /// first admission protects the object's first cached version.
    pub fn is_pinned(&self, object: Hash32) -> bool {
        self.pins.contains(&object)
    }

    /// Pin an object so none of its entries can be evicted.
    ///
    /// Pinning an object that is already cached never makes the cache exceed
    /// its byte bound: if the pinned set would not fit, the pin is refused with
    /// `LimitExceeded`.
    pub fn pin(&mut self, object: Hash32) -> Result<()> {
        if self.pins.contains(&object) {
            return Ok(());
        }
        if self.pins.len() >= self.options.max_pins {
            return Err(Error::limit_exceeded(format!(
                "object cache pin set is full ({} pins); refusing to pin another object",
                self.options.max_pins
            )));
        }
        self.pins.insert(object);
        Ok(())
    }

    /// Remove a pin. The entry stays until normal eviction selects it.
    pub fn unpin(&mut self, object: Hash32) {
        self.pins.remove(&object);
    }

    /// Number of pins.
    pub fn pin_count(&self) -> usize {
        self.pins.len()
    }

    /// Look up one verified decoded version. Counts a hit or a miss.
    pub fn lookup(&mut self, key: CacheKey) -> Option<Arc<Vec<u8>>> {
        if !self.enabled() {
            return None;
        }
        match self.entries.get(&key) {
            Some(entry) => {
                self.hits += 1;
                self.bytes_served = self.bytes_served.saturating_add(entry.bytes.len() as u64);
                Some(Arc::clone(&entry.bytes))
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }

    /// Admit verified decoded bytes. Returns `true` when the value is now
    /// cached. A `false` return is not an error: the read path simply does not
    /// benefit next time.
    pub fn admit(&mut self, key: CacheKey, bytes: Arc<Vec<u8>>, score: CacheScore) -> bool {
        if !self.enabled() {
            return false;
        }
        let size = bytes.len() as u64;
        if size > self.options.max_bytes {
            self.rejections += 1;
            return false;
        }
        // A new version of the same object makes the old entry unreachable.
        let stale: Vec<CacheKey> = self
            .entries
            .keys()
            .copied()
            .filter(|existing| existing.object == key.object && existing.content != key.content)
            .collect();
        for existing in stale {
            self.remove(&existing, true);
        }
        if self.entries.contains_key(&key) {
            // Re-admission of the same version (for example a coalesced waiter
            // admitting the leader's result): refresh the score, but do not
            // count a second admission.
            if let Some(entry) = self.entries.get_mut(&key) {
                entry.score = score;
                entry.bytes = bytes;
            }
            return true;
        }
        // Make room. Never evict a pinned entry; reject when only pinned
        // entries remain.
        while self.bytes.saturating_add(size) > self.options.max_bytes
            || self.entries.len() >= self.options.max_entries
        {
            match self.eviction_candidate() {
                Some(victim) => self.remove(&victim, false),
                None => {
                    if !self.pins.is_empty() {
                        self.pinned_skips = self.pinned_skips.saturating_add(1);
                    }
                    self.rejections += 1;
                    return false;
                }
            }
        }
        self.entries.insert(key, Entry { bytes, score });
        self.bytes = self.bytes.saturating_add(size);
        self.admissions += 1;
        true
    }

    /// Drop every entry (pins are retained). Used when a caller wants a cold
    /// comparison; the store never calls it implicitly.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    fn remove(&mut self, key: &CacheKey, stale: bool) {
        if let Some(entry) = self.entries.remove(key) {
            self.bytes = self.bytes.saturating_sub(entry.bytes.len() as u64);
            if stale {
                self.stale_removals += 1;
            } else {
                self.evictions += 1;
            }
        }
    }

    fn fit_to_bound(&mut self) {
        while self.bytes > self.options.max_bytes || self.entries.len() > self.options.max_entries {
            match self.eviction_candidate() {
                Some(victim) => self.remove(&victim, false),
                None => {
                    self.pinned_skips = self.pinned_skips.saturating_add(1);
                    break;
                }
            }
        }
    }

    /// The next entry to evict under the documented order, or `None` when only
    /// pinned entries remain.
    fn eviction_candidate(&self) -> Option<CacheKey> {
        let mut best: Option<(CacheKey, u128, u64)> = None;
        for (key, entry) in &self.entries {
            if self.pins.contains(&key.object) {
                continue;
            }
            let size = entry.bytes.len() as u64;
            let cost = entry.score.expected_ns();
            let replace = match &best {
                None => true,
                Some((best_key, best_cost, best_size)) => {
                    evicts_before(cost, size, key, *best_cost, *best_size, best_key)
                }
            };
            if replace {
                best = Some((*key, cost, size));
            }
        }
        best.map(|(key, _, _)| key)
    }

    /// The admission score for one object version, using the measured cost
    /// table's row for `class`.
    pub fn score_for(
        &self,
        class: CostClass,
        decoded_bytes: u64,
        dependents: u64,
        reads: u64,
    ) -> CacheScore {
        CacheScore {
            decode_ns: cost_model::estimate_ns(class, decoded_bytes, self.options.cost_model),
            dependents,
            reads: reads.max(1),
        }
    }
}

/// Total order: `true` when candidate `a` must be evicted before `b`.
fn evicts_before(
    cost_a: u128,
    size_a: u64,
    key_a: &CacheKey,
    cost_b: u128,
    size_b: u64,
    key_b: &CacheKey,
) -> bool {
    // Exact comparison of cost/size without division.
    let lhs = cost_a.saturating_mul(size_b as u128);
    let rhs = cost_b.saturating_mul(size_a as u128);
    if lhs != rhs {
        return lhs < rhs;
    }
    if cost_a != cost_b {
        return cost_a < cost_b;
    }
    if size_a != size_b {
        return size_a > size_b;
    }
    key_a < key_b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(byte: u8) -> Hash32 {
        let mut raw = [0u8; 32];
        raw[0] = byte;
        raw[31] = byte;
        Hash32::from_bytes(raw)
    }

    fn bytes(len: usize, fill: u8) -> Arc<Vec<u8>> {
        Arc::new(vec![fill; len])
    }

    fn score(decode_ns: u64, dependents: u64, reads: u64) -> CacheScore {
        CacheScore {
            decode_ns,
            dependents,
            reads,
        }
    }

    #[test]
    fn disabled_by_default() {
        let mut cache = ObjectCache::new(ObjectCacheOptions::default());
        assert!(!cache.enabled());
        assert!(!cache.admit(CacheKey::new(h(1), h(2)), bytes(10, 1), score(1, 1, 1)));
        assert!(cache.lookup(CacheKey::new(h(1), h(2))).is_none());
        let stats = cache.stats();
        assert_eq!(stats.entries, 0);
        assert_eq!(stats.admissions, 0);
    }

    #[test]
    fn eviction_is_deterministic_and_prefers_cheap_entries() {
        let mut cache = ObjectCache::new(ObjectCacheOptions {
            max_bytes: 20,
            ..ObjectCacheOptions::default()
        });
        // Two 10-byte entries fit; the third forces one out.
        assert!(cache.admit(CacheKey::new(h(1), h(1)), bytes(10, 1), score(100, 5, 1)));
        assert!(cache.admit(CacheKey::new(h(2), h(2)), bytes(10, 2), score(100, 1, 1)));
        assert!(cache.admit(CacheKey::new(h(3), h(3)), bytes(10, 3), score(100, 9, 1)));
        // The lowest cost/byte entry (object 2) was evicted.
        assert!(cache.lookup(CacheKey::new(h(2), h(2))).is_none());
        assert!(cache.lookup(CacheKey::new(h(1), h(1))).is_some());
        assert!(cache.lookup(CacheKey::new(h(3), h(3))).is_some());
        assert_eq!(cache.stats().evictions, 1);
    }

    #[test]
    fn a_score_tie_with_zero_value_evicts_the_larger_entry() {
        let mut cache = ObjectCache::new(ObjectCacheOptions {
            max_bytes: 20,
            ..ObjectCacheOptions::default()
        });
        // Zero dependents means zero expected value for every entry, so the
        // documented size tie-break decides: the largest entry goes first.
        assert!(cache.admit(CacheKey::new(h(1), h(1)), bytes(16, 1), score(8, 0, 1)));
        assert!(cache.admit(CacheKey::new(h(2), h(2)), bytes(4, 2), score(2, 0, 1)));
        assert!(cache.admit(CacheKey::new(h(3), h(3)), bytes(8, 3), score(4, 0, 1)));
        assert_eq!(cache.stats().bytes, 12);
        assert!(cache.lookup(CacheKey::new(h(1), h(1))).is_none());
        assert!(cache.lookup(CacheKey::new(h(2), h(2))).is_some());
        assert!(cache.lookup(CacheKey::new(h(3), h(3))).is_some());
    }

    #[test]
    fn pinned_entry_is_never_evicted_and_admission_is_refused() {
        let mut cache = ObjectCache::new(ObjectCacheOptions {
            max_bytes: 20,
            ..ObjectCacheOptions::default()
        });
        cache.pin(h(1)).unwrap();
        assert!(cache.admit(CacheKey::new(h(1), h(1)), bytes(12, 1), score(10, 0, 1)));
        assert!(!cache.admit(CacheKey::new(h(2), h(2)), bytes(12, 2), score(1_000, 10, 1)));
        assert!(cache.lookup(CacheKey::new(h(1), h(1))).is_some());
        assert!(cache.lookup(CacheKey::new(h(2), h(2))).is_none());
        assert_eq!(cache.stats().rejections, 1);
        assert_eq!(cache.stats().pinned_skips, 1);
    }

    #[test]
    fn a_new_version_removes_the_old_version() {
        let mut cache = ObjectCache::new(ObjectCacheOptions {
            max_bytes: 1024,
            ..ObjectCacheOptions::default()
        });
        let old = CacheKey::new(h(1), h(1));
        let new = CacheKey::new(h(1), h(2));
        assert!(cache.admit(old, bytes(10, 1), score(1, 1, 1)));
        assert!(cache.admit(new, bytes(10, 2), score(1, 1, 1)));
        assert!(cache.lookup(old).is_none());
        assert!(cache.lookup(new).is_some());
        assert_eq!(cache.stats().stale_removals, 1);
        assert_eq!(cache.stats().entries, 1);
    }

    #[test]
    fn shrinking_the_bound_evicts_down_to_it() {
        let mut cache = ObjectCache::new(ObjectCacheOptions {
            max_bytes: 1024,
            ..ObjectCacheOptions::default()
        });
        for byte in 1..=4u8 {
            assert!(cache.admit(
                CacheKey::new(h(byte), h(byte)),
                bytes(100, byte),
                score(10 * byte as u64, 1, 1)
            ));
        }
        assert_eq!(cache.stats().bytes, 400);
        cache.set_options(ObjectCacheOptions {
            max_bytes: 100,
            ..ObjectCacheOptions::default()
        });
        assert_eq!(cache.stats().bytes, 100);
        // The highest cost/byte entry (object 4) survived.
        assert!(cache.lookup(CacheKey::new(h(4), h(4))).is_some());
    }
}
