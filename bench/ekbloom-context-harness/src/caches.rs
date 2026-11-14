//! Two strong conventional bounded caches over certified decisions.
//!
//! Both use a fixed 256-byte arena slot, so the retained-byte accounting is a
//! declared function of capacity and not an allocator accident. Both return a
//! copy of the cached decision; the arm performs the post-lookup checks before
//! trusting it.

use std::collections::HashMap;

pub const ENTRY_BYTES: u64 = 256;
const INDEX_BYTES: u64 = 32;
const NIL: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, Default)]
pub struct Entry {
    pub key: [u8; 32],
    pub subject: [u8; 32],
    pub contract: [u8; 32],
    pub page_id: [u8; 32],
    pub content: [u8; 32],
    pub snapshot: [u8; 32],
    pub authority: [u8; 32],
    pub epoch: u64,
    pub verdict: u8,
    pub reason: u16,
}

#[derive(Clone, Copy, Debug, Default)]
struct Slot {
    entry: Entry,
    prev: u32,
    next: u32,
    /// 0 = unused, 1 = window, 2 = probation, 3 = protected, 4 = LRU.
    list: u8,
}

struct List {
    head: u32,
    tail: u32,
    len: usize,
}

impl List {
    fn new() -> Self {
        List {
            head: NIL,
            tail: NIL,
            len: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub admissions: u64,
    pub evictions: u64,
    pub rejections: u64,
}

/// A byte-budgeted LRU.
pub struct LruCache {
    slots: Vec<Slot>,
    index: HashMap<[u8; 32], u32>,
    free: Vec<u32>,
    order: List,
    len: usize,
    budget: u64,
    stats: CacheStats,
}

impl LruCache {
    pub fn with_budget(budget: u64) -> Self {
        Self::with_capacity(capacity_for(budget))
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let mut slots = vec![Slot::default(); capacity];
        let mut free = Vec::with_capacity(capacity);
        for i in (0..capacity).rev() {
            free.push(i as u32);
        }
        LruCache {
            slots,
            index: HashMap::with_capacity(capacity.saturating_mul(2)),
            free,
            order: List::new(),
            len: 0,
            budget: capacity as u64 * ENTRY_BYTES,
            stats: CacheStats::default(),
        }
    }

    fn unlink(&mut self, i: u32) {
        let (prev, next) = (self.slots[i as usize].prev, self.slots[i as usize].next);
        if prev != NIL {
            self.slots[prev as usize].next = next;
        } else {
            self.order.head = next;
        }
        if next != NIL {
            self.slots[next as usize].prev = prev;
        } else {
            self.order.tail = prev;
        }
        let slot = &mut self.slots[i as usize];
        slot.prev = NIL;
        slot.next = NIL;
        self.order.len -= 1;
    }

    fn push_front(&mut self, i: u32) {
        let old = self.order.head;
        {
            let slot = &mut self.slots[i as usize];
            slot.prev = NIL;
            slot.next = old;
        }
        if old != NIL {
            self.slots[old as usize].prev = i;
        } else {
            self.order.tail = i;
        }
        self.order.head = i;
        self.order.len += 1;
    }

    fn evict_tail(&mut self) -> Option<Entry> {
        let tail = self.order.tail;
        if tail == NIL {
            return None;
        }
        let entry = self.slots[tail as usize].entry;
        self.unlink(tail);
        let key = entry.key;
        self.index.remove(&key);
        let slot = &mut self.slots[tail as usize];
        slot.list = 0;
        slot.entry = Entry::default();
        self.free.push(tail);
        self.len -= 1;
        self.stats.evictions += 1;
        Some(entry)
    }

    pub fn get(&mut self, key: &[u8; 32]) -> Option<Entry> {
        let Some(&i) = self.index.get(key) else {
            self.stats.misses += 1;
            return None;
        };
        self.stats.hits += 1;
        let entry = self.slots[i as usize].entry;
        self.unlink(i);
        self.push_front(i);
        Some(entry)
    }

    pub fn insert(&mut self, entry: Entry) {
        if let Some(&i) = self.index.get(&entry.key) {
            self.slots[i as usize].entry = entry;
            self.unlink(i);
            self.push_front(i);
            return;
        }
        self.stats.admissions += 1;
        let i = if let Some(i) = self.free.pop() {
            i
        } else {
            let _ = self.evict_tail();
            // The eviction pushed a free slot.
            self.free.pop().expect("eviction frees a slot")
        };
        self.slots[i as usize] = Slot {
            entry,
            prev: NIL,
            next: NIL,
            list: 4,
        };
        self.index.insert(entry.key, i);
        self.push_front(i);
        self.len += 1;
    }

    pub fn set_budget(&mut self, budget: u64) {
        self.budget = budget;
        let capacity = capacity_for(budget);
        while self.len > capacity {
            if self.evict_tail().is_none() {
                break;
            }
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.slots
            .iter()
            .filter(|slot| slot.list != 0)
            .map(|slot| &slot.entry)
    }

    fn entries_mru(&self) -> Vec<Entry> {
        let mut out = Vec::with_capacity(self.len);
        let mut cursor = self.order.head;
        while cursor != NIL {
            out.push(self.slots[cursor as usize].entry);
            cursor = self.slots[cursor as usize].next;
        }
        out
    }

    pub fn retained_bytes(&self) -> u64 {
        self.slots.len() as u64 * ENTRY_BYTES + self.index.capacity() as u64 * INDEX_BYTES
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    pub fn stats(&self) -> CacheStats {
        self.stats
    }
}

const SKETCH_ROWS: usize = 4;
const SKETCH_COUNTERS: usize = 16_384;
const SKETCH_BYTES: u64 = (SKETCH_ROWS * SKETCH_COUNTERS / 2) as u64;

/// A byte-budgeted W-TinyLFU: a 1 % window LRU, a 20/80 probation/protected
/// main cache, and a 4-row 4-bit Count-Min sketch for admission.
pub struct TinyLfu {
    slots: Vec<Slot>,
    index: HashMap<[u8; 32], u32>,
    free: Vec<u32>,
    window: List,
    probation: List,
    protected: List,
    sketch: Vec<u8>,
    sketch_increments: u64,
    sketch_reset_at: u64,
    len: usize,
    budget: u64,
    capacity: usize,
    window_cap: usize,
    protected_cap: usize,
    stats: CacheStats,
}

fn index_capacity(capacity: usize) -> usize {
    HashMap::<[u8; 32], u32>::with_capacity(capacity).capacity()
}

/// Size the arena so the *measured* retained bytes (arena + the hash map's
/// actual capacity + an optional sketch) fit the declared budget. This is an
/// exact fit, not an estimate: the map's rounding is included in the search.
fn fit_capacity(budget: u64, sketch_bytes: u64) -> usize {
    let mut capacity =
        (((budget.saturating_sub(sketch_bytes)).max(ENTRY_BYTES) / ENTRY_BYTES) as usize).max(1);
    let fits = |cap: usize| -> bool {
        // The cache allocates its index at twice the arena capacity, so the
        // map never rehashes while the arena fills. The accounting charges the
        // map's actual capacity, which is what `retained_bytes` reports.
        let index_cap = index_capacity(cap.saturating_mul(2));
        cap <= index_cap
            && cap as u64 * ENTRY_BYTES + index_cap as u64 * INDEX_BYTES + sketch_bytes <= budget
    };
    for _ in 0..32 {
        let total = capacity as u64 * ENTRY_BYTES
            + index_capacity(capacity.saturating_mul(2)) as u64 * INDEX_BYTES
            + sketch_bytes;
        if fits(capacity) {
            // Fill the remaining gap: the hash map's capacity jumps in lumps,
            // so increase one entry at a time until the next jump would exceed
            // the budget. This makes the measured retained bytes approach the
            // declared budget instead of leaving a lump-sized hole.
            for _ in 0..2048 {
                let next = capacity + 1;
                if !fits(next) {
                    break;
                }
                capacity = next;
            }
            return capacity;
        }
        let over = total.saturating_sub(budget);
        let step = ((over + ENTRY_BYTES - 1) / ENTRY_BYTES).max(1) as usize;
        capacity = capacity.saturating_sub(step);
        if capacity == 0 {
            return 1;
        }
    }
    1
}

fn capacity_for(budget: u64) -> usize {
    fit_capacity(budget, 0)
}

fn tiny_capacity_for(budget: u64) -> usize {
    fit_capacity(budget, SKETCH_BYTES)
}

fn sketch_index(key: &[u8; 32], row: usize) -> usize {
    let mut x = u64::from_le_bytes(key[row * 8..row * 8 + 8].try_into().unwrap());
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    x ^= x >> 33;
    (x as usize) & (SKETCH_COUNTERS - 1)
}

impl TinyLfu {
    pub fn with_budget(budget: u64) -> Self {
        let capacity = tiny_capacity_for(budget);
        let window_cap = (capacity / 100).max(1).min(capacity);
        let main = capacity - window_cap;
        let protected_cap = main * 4 / 5;
        let mut slots = vec![Slot::default(); capacity];
        let mut free = Vec::with_capacity(capacity);
        for i in (0..capacity).rev() {
            free.push(i as u32);
        }
        TinyLfu {
            slots,
            index: HashMap::with_capacity(capacity.saturating_mul(2)),
            free,
            window: List::new(),
            probation: List::new(),
            protected: List::new(),
            sketch: vec![0u8; SKETCH_ROWS * SKETCH_COUNTERS / 2],
            sketch_increments: 0,
            sketch_reset_at: (capacity as u64 * 10).max(1024),
            len: 0,
            budget,
            capacity,
            window_cap,
            protected_cap,
            stats: CacheStats::default(),
        }
    }

    fn list_head(&self, list: u8) -> u32 {
        match list {
            1 => self.window.head,
            2 => self.probation.head,
            _ => self.protected.head,
        }
    }

    fn list_tail(&self, list: u8) -> u32 {
        match list {
            1 => self.window.tail,
            2 => self.probation.tail,
            _ => self.protected.tail,
        }
    }

    fn set_list_ends(&mut self, list: u8, head: u32, tail: u32, len_delta: isize) {
        match list {
            1 => {
                self.window.head = head;
                self.window.tail = tail;
                self.window.len = (self.window.len as isize + len_delta) as usize;
            }
            2 => {
                self.probation.head = head;
                self.probation.tail = tail;
                self.probation.len = (self.probation.len as isize + len_delta) as usize;
            }
            _ => {
                self.protected.head = head;
                self.protected.tail = tail;
                self.protected.len = (self.protected.len as isize + len_delta) as usize;
            }
        }
    }

    fn unlink(&mut self, i: u32) {
        let list_id = self.slots[i as usize].list;
        if list_id == 0 {
            return;
        }
        let (prev, next) = (self.slots[i as usize].prev, self.slots[i as usize].next);
        let mut head = self.list_head(list_id);
        let mut tail = self.list_tail(list_id);
        if prev != NIL {
            self.slots[prev as usize].next = next;
        } else {
            head = next;
        }
        if next != NIL {
            self.slots[next as usize].prev = prev;
        } else {
            tail = prev;
        }
        self.slots[i as usize].prev = NIL;
        self.slots[i as usize].next = NIL;
        self.set_list_ends(list_id, head, tail, -1);
    }

    fn push_front(&mut self, i: u32, list_id: u8) {
        self.slots[i as usize].list = list_id;
        let old = self.list_head(list_id);
        let mut tail = self.list_tail(list_id);
        {
            let slot = &mut self.slots[i as usize];
            slot.prev = NIL;
            slot.next = old;
        }
        if old != NIL {
            self.slots[old as usize].prev = i;
        } else {
            tail = i;
        }
        self.set_list_ends(list_id, i, tail, 1);
    }

    fn detach(&mut self, i: u32) -> Entry {
        let entry = self.slots[i as usize].entry;
        self.unlink(i);
        self.index.remove(&entry.key);
        let slot = &mut self.slots[i as usize];
        slot.list = 0;
        slot.entry = Entry::default();
        self.free.push(i);
        self.len -= 1;
        entry
    }

    fn increment(&mut self, key: &[u8; 32]) {
        for row in 0..SKETCH_ROWS {
            let idx = sketch_index(key, row);
            let byte = idx / 2;
            let shift = (idx % 2) * 4;
            let slot = &mut self.sketch[row * (SKETCH_COUNTERS / 2) + byte];
            let value = (*slot >> shift) & 0x0f;
            if value < 15 {
                *slot = (*slot & !(0x0f << shift)) | ((value + 1) << shift);
            }
        }
        self.sketch_increments += 1;
        if self.sketch_increments >= self.sketch_reset_at {
            for byte in &mut self.sketch {
                *byte = (*byte >> 1) & 0x77;
            }
            self.sketch_increments = 0;
        }
    }

    fn frequency(&self, key: &[u8; 32]) -> u8 {
        let mut min = 15u8;
        for row in 0..SKETCH_ROWS {
            let idx = sketch_index(key, row);
            let byte = self.sketch[row * (SKETCH_COUNTERS / 2) + idx / 2];
            let value = (byte >> ((idx % 2) * 4)) & 0x0f;
            min = min.min(value);
        }
        min
    }

    fn enforce_protected(&mut self) {
        while self.protected.len > self.protected_cap && self.protected.len > 0 {
            let victim = self.protected.tail;
            if victim == NIL {
                break;
            }
            self.unlink(victim);
            self.push_front(victim, 2);
        }
    }

    pub fn get(&mut self, key: &[u8; 32]) -> Option<Entry> {
        self.increment(key);
        let Some(&i) = self.index.get(key) else {
            self.stats.misses += 1;
            return None;
        };
        let entry = self.slots[i as usize].entry;
        let list = self.slots[i as usize].list;
        self.stats.hits += 1;
        if list == 3 {
            self.unlink(i);
            self.push_front(i, 3);
        } else if list == 2 {
            self.unlink(i);
            self.push_front(i, 3);
            self.enforce_protected();
        } else {
            self.unlink(i);
            self.push_front(i, 1);
        }
        Some(entry)
    }

    pub fn insert(&mut self, entry: Entry) {
        self.increment(&entry.key);
        if let Some(&i) = self.index.get(&entry.key) {
            self.slots[i as usize].entry = entry;
            let list = self.slots[i as usize].list;
            if list == 2 {
                self.unlink(i);
                self.push_front(i, 3);
                self.enforce_protected();
            } else {
                self.unlink(i);
                self.push_front(i, if list == 0 { 1 } else { list });
            }
            return;
        }
        self.stats.admissions += 1;
        let slot = if let Some(i) = self.free.pop() {
            i
        } else {
            // The cache is full. Take the window LRU as the admission candidate.
            if self.window.len > 0 && self.window.tail != NIL {
                let candidate = self.window.tail;
                self.unlink(candidate);
                let candidate_key = self.slots[candidate as usize].entry.key;
                let main_cap = self.capacity.saturating_sub(self.window_cap);
                let main_len = self.probation.len + self.protected.len;
                if main_len < main_cap {
                    // There is main space; the candidate moves there and the
                    // new entry takes the newly free slot (which must exist
                    // because total < capacity in this branch).
                    self.push_front(candidate, 2);
                    if let Some(i) = self.free.pop() {
                        i
                    } else {
                        // Defensive: evict a main victim to make room.
                        let victim = if self.probation.tail != NIL {
                            self.probation.tail
                        } else {
                            self.protected.tail
                        };
                        if victim == NIL {
                            self.stats.rejections += 1;
                            return;
                        }
                        let _ = self.detach(victim);
                        self.free.pop().expect("a detached victim frees a slot")
                    }
                } else {
                    let victim = if self.probation.len > 0 {
                        self.probation.tail
                    } else {
                        self.protected.tail
                    };
                    if victim != NIL
                        && self.frequency(&candidate_key)
                            > self.frequency(&self.slots[victim as usize].entry.key)
                    {
                        let _ = self.detach(victim);
                        self.push_front(candidate, 2);
                        self.free.pop().expect("a detached victim frees a slot")
                    } else {
                        let _ = self.detach(candidate);
                        self.free.pop().expect("a detached candidate frees a slot")
                    }
                }
            } else {
                // No window entry: admit the new key against the main victim.
                let victim = if self.probation.len > 0 {
                    self.probation.tail
                } else {
                    self.protected.tail
                };
                if victim == NIL {
                    self.stats.rejections += 1;
                    return;
                }
                if self.frequency(&entry.key)
                    > self.frequency(&self.slots[victim as usize].entry.key)
                {
                    let _ = self.detach(victim);
                    self.free.pop().expect("a detached victim frees a slot")
                } else {
                    self.stats.rejections += 1;
                    return;
                }
            }
        };
        self.slots[slot as usize] = Slot {
            entry,
            prev: NIL,
            next: NIL,
            list: 0,
        };
        self.index.insert(entry.key, slot);
        self.push_front(slot, 1);
        self.len += 1;
    }

    pub fn set_budget(&mut self, budget: u64) {
        self.budget = budget;
        let capacity = tiny_capacity_for(budget);
        let window_cap = (capacity / 100).max(1).min(capacity);
        let main = capacity - window_cap;
        self.capacity = capacity;
        self.window_cap = window_cap;
        self.protected_cap = main * 4 / 5;
        self.sketch_reset_at = (capacity as u64 * 10).max(1024);
        while self.len > capacity {
            let victim = if self.window.len > self.window_cap && self.window.tail != NIL {
                self.window.tail
            } else if self.probation.len > 0 {
                self.probation.tail
            } else if self.protected.len > 0 {
                self.protected.tail
            } else {
                NIL
            };
            if victim == NIL {
                break;
            }
            let _ = self.detach(victim);
            self.stats.evictions += 1;
        }
        self.enforce_protected();
    }

    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.slots
            .iter()
            .filter(|slot| slot.list != 0)
            .map(|slot| &slot.entry)
    }

    fn entries_mru(&self) -> Vec<Entry> {
        let mut out = Vec::with_capacity(self.len);
        for list_id in [1u8, 3, 2] {
            let mut cursor = match list_id {
                1 => self.window.head,
                2 => self.probation.head,
                _ => self.protected.head,
            };
            while cursor != NIL {
                out.push(self.slots[cursor as usize].entry);
                cursor = self.slots[cursor as usize].next;
            }
        }
        out
    }

    pub fn retained_bytes(&self) -> u64 {
        self.capacity as u64 * ENTRY_BYTES
            + self.index.capacity() as u64 * INDEX_BYTES
            + SKETCH_BYTES
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn stats(&self) -> CacheStats {
        self.stats
    }
}

pub enum Cache {
    Lru(LruCache),
    Tiny(TinyLfu),
}

impl Cache {
    pub fn new(variant: &str, budget: u64) -> Result<Self, String> {
        match variant {
            "lru" => Ok(Cache::Lru(LruCache::with_budget(budget))),
            "tinylfu" => Ok(Cache::Tiny(TinyLfu::with_budget(budget))),
            other => Err(format!("unknown cache variant `{other}`")),
        }
    }

    pub fn get(&mut self, key: &[u8; 32]) -> Option<Entry> {
        match self {
            Cache::Lru(c) => c.get(key),
            Cache::Tiny(c) => c.get(key),
        }
    }

    pub fn insert(&mut self, entry: Entry) {
        match self {
            Cache::Lru(c) => c.insert(entry),
            Cache::Tiny(c) => c.insert(entry),
        }
    }

    pub fn set_budget(&mut self, budget: u64) {
        match self {
            Cache::Lru(c) => c.set_budget(budget),
            Cache::Tiny(c) => c.set_budget(budget),
        }
    }

    pub fn retained_bytes(&self) -> u64 {
        match self {
            Cache::Lru(c) => c.retained_bytes(),
            Cache::Tiny(c) => c.retained_bytes(),
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Cache::Lru(c) => c.len(),
            Cache::Tiny(c) => c.len(),
        }
    }

    pub fn stats(&self) -> CacheStats {
        match self {
            Cache::Lru(c) => c.stats(),
            Cache::Tiny(c) => c.stats(),
        }
    }

    pub fn entries(&self) -> Vec<Entry> {
        match self {
            Cache::Lru(c) => c.iter().copied().collect(),
            Cache::Tiny(c) => c.iter().copied().collect(),
        }
    }

    /// Reallocate the arena at a new budget, preserving recency order. This is
    /// a real resize: the retained-byte accounting follows the new arena, so a
    /// caller that gives space to a manifestation actually gives it up.
    pub fn resize(&mut self, budget: u64) {
        let variant = match self {
            Cache::Lru(_) => "lru",
            Cache::Tiny(_) => "tinylfu",
        };
        let entries = match self {
            Cache::Lru(c) => c.entries_mru(),
            Cache::Tiny(c) => c.entries_mru(),
        };
        let mut next = Cache::new(variant, budget).expect("known cache variant");
        for entry in entries {
            next.insert(entry);
        }
        *self = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(n: u8) -> Entry {
        let mut e = Entry::default();
        e.key = [n; 32];
        e.verdict = n % 3;
        e
    }

    #[test]
    fn lru_evicts_the_least_recently_used() {
        let mut c = LruCache::with_capacity(2);
        c.insert(entry(1));
        c.insert(entry(2));
        assert!(c.get(&[1u8; 32]).is_some());
        c.insert(entry(3));
        // 1 was touched, so 2 is the victim.
        assert!(c.get(&[2u8; 32]).is_none());
        assert!(c.get(&[1u8; 32]).is_some());
        assert!(c.get(&[3u8; 32]).is_some());
    }

    #[test]
    fn tinylfu_keeps_a_hot_entry_against_a_cold_admission() {
        let mut c = TinyLfu::with_budget(200 * (ENTRY_BYTES + 2 * INDEX_BYTES) + SKETCH_BYTES);
        for n in 0..100u8 {
            c.insert(entry(n));
        }
        // Make entry 7 hot, then churn cold entries.
        for _ in 0..200 {
            let _ = c.get(&[7u8; 32]);
        }
        for n in 100..250u8 {
            c.insert(entry(n));
        }
        assert!(c.get(&[7u8; 32]).is_some());
    }

    #[test]
    fn shrinking_the_budget_evicts_down_to_it() {
        let mut c = LruCache::with_budget(10 * (ENTRY_BYTES + 2 * INDEX_BYTES));
        for n in 0..10u8 {
            c.insert(entry(n));
        }
        c.set_budget(2 * (ENTRY_BYTES + 2 * INDEX_BYTES));
        assert!(c.len() <= 2);
    }
}
