//! Distinct names: one table shared by every worker gives each distinct name key a dense id (0
//! upward); each group keeps a bitset over those ids (pass2.rs), so names_distinct counts its bits.
//!
//! A key is a name's raw bytes between its quotes, escapes kept: `""` -> `"` is one to one, so raw
//! keys are equal exactly when the values are, and an empty unquoted name and `""` are both the
//! empty key (c-join.md:34).

use std::arch::x86_64::{_MM_HINT_T0, _bzhi_u64, _mm_prefetch};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::records::load;

/// 64 x 64 -> 128-bit multiply, folded to 64 bits (wyhash's mum).
fn mum(a: u64, b: u64) -> u64 {
    let x = a as u128 * b as u128;
    x as u64 ^ (x >> 64) as u64
}

/// The key's word `k` (bytes 8k..8k+8) with the bytes past the key cleared: BMI2's bzhi keeps the
/// low `8 * len - 64 * k` bits (all 64 from a count of 64 to 255, none at 0), so no branch on the
/// length. bzhi reads only the count's low byte, so the word is right up to 24 bytes of key, and
/// `key` uses its words only then.
///
/// # Safety
/// The 8 bytes at `p + at + 8k` must lie inside the padded mapping.
#[inline(always)]
unsafe fn key_word(p: *const u8, at: usize, len: usize, k: usize) -> u64 {
    unsafe { _bzhi_u64(load::<u64>(p, at + 8 * k), (8 * len).saturating_sub(64 * k) as u32) }
}

/// A name key as the table sees it, five scalars passed by value so they stay in registers
/// (880ff14 passed a 6-word struct by reference: it was spilled to the stack and reloaded around
/// every lookup). Up to 24 bytes: the hash, the three zero-padded words and the length. Longer: the
/// hash, then the key's offset in the mapping and its length in the words, and 25 as the length.
#[derive(Clone, Copy)]
pub struct Key {
    h: u64,
    w: [u64; 3],
    len: u64,
}

/// The key `p[at..at + len]`. Up to 24 bytes (nearly every name, c-join.md 2.2): three zero-padded
/// words and two multiplies, no loop and no branch; the words are also what the table compares.
///
/// # Safety
/// The key and 24 bytes from its start must lie inside the padded mapping.
#[inline(always)]
pub unsafe fn key(p: *const u8, at: usize, len: usize) -> Key {
    if len > 24 {
        return unsafe { long_key(p, at, len) };
    }
    let w = unsafe { [key_word(p, at, len, 0), key_word(p, at, len, 1), key_word(p, at, len, 2)] };
    let h = mum(w[0] ^ 0xa076_1d64_78bd_642f ^ len as u64, w[1] ^ 0xe703_7ed1_a0b4_28db) ^ mum(w[2] ^ 0x8ebc_6af0_9c88_c6e3, 0x5899_65cc_7537_4cc3);
    Key { h, w, len: len as u64 }
}

/// `key` for keys over 24 bytes: one multiply per 8-byte word.
#[cold]
#[inline(never)]
unsafe fn long_key(p: *const u8, at: usize, len: usize) -> Key {
    // Word k of the whole key is word 0 of the key from 8k on, at most 8 bytes of it (count <= 64).
    let word = |k: usize| unsafe { key_word(p, at + 8 * k, (len - 8 * k).min(8), 0) };
    let h = (0..len.div_ceil(8)).fold(len as u64, |h, k| mum(h ^ word(k), 0x8ebc_6af0_9c88_c6e3));
    Key { h, w: [at as u64, len as u64, 0], len: 25 }
}

/// One entry's key and id: the key's three words (a key over 24 bytes keeps its first occurrence's
/// file offset and its length there instead), then min(length, 25) << 32 | id.
struct Slot {
    key: [AtomicU64; 3],
    meta: AtomicU64,
}

/// A bucket's four entries, two cache lines; their tags live apart, in `Table::tags`.
#[repr(align(64))]
struct Slots([Slot; 4]);

/// Tag lanes: a bucket's tag word holds four 16-bit lanes, one per entry, filled in order: 0 empty,
/// 1 PENDING while its inserter writes the entry, else the key's tag (16 hash bits, at least 2).
const PENDING: u64 = 1;
const LANES: u64 = 0x0001_0001_0001_0001;

/// Name key -> dense id, shared by all workers, lock-free. Open addressing over buckets of four
/// entries from the key's home bucket (the hash's high bits, by multiply-shift). A lookup loads the
/// bucket's tag word, matches its four lanes against the key's tag at once (SWAR), and compares the
/// key only in a matching entry: about one key compare per lookup. With two entries per bucket
/// and a state word each (880ff14), a lookup examined 2.74 entries on average at the starting
/// 77 % load, p99 22, p99.9 47 (full seed 1, a counting build, c-42 step 2 (a)): each a load, a
/// compare and a branch whose count varied. The 32K tag words (256 KiB) mostly stay in L2.
///
/// An entry is claimed by a compare and swap of its lane from empty to PENDING, filled, then
/// published by adding tag - 1 to its lane with Release, so a reader that Acquire-loads the tag
/// word and sees the tag sees the whole entry. Nothing is ever moved or deleted, so every key's
/// probe sequence is fixed and two workers inserting one new key meet at the same lane: one id.
///
/// The table never grows: past 7/8 full it raises `overflow` and stops inserting, and main reruns
/// the pass with 4 times the buckets (as the e1 rerun does), so any input stays exact.
pub struct Table {
    tags: Box<[AtomicU64]>,
    slots: Box<[Slots]>,
    limit: u32,
    next_id: AtomicU32,
    overflow: AtomicBool,
}

impl Table {
    /// A table of `buckets` buckets (4 entries each), all empty. Allocated zeroed, so its pages
    /// are only touched (and charged) as entries land in them.
    pub fn new(buckets: usize) -> Table {
        let buckets = buckets.max(1);
        // SAFETY: all-zero tag words and slots are valid: atomics of 0, every lane empty.
        let (tags, slots) = unsafe {
            (Box::<[AtomicU64]>::new_zeroed_slice(buckets).assume_init(), Box::<[Slots]>::new_zeroed_slice(buckets).assume_init())
        };
        let cap = 4 * buckets;
        Table { tags, slots, limit: (cap - cap / 8) as u32, next_id: AtomicU32::new(0), overflow: AtomicBool::new(false) }
    }

    /// Whether some insert found the table 7/8 full: the pass that used it must be rerun, larger.
    pub fn overflowed(&self) -> bool {
        self.overflow.load(Ordering::Relaxed)
    }

    /// The bucket count, for a rerun's 4x.
    pub fn buckets(&self) -> usize {
        self.tags.len()
    }

    /// The entry count: every id handed out is below it.
    pub fn entries(&self) -> usize {
        4 * self.tags.len()
    }

    /// The table's base pointers and size as plain values, so the parse loop holds them in
    /// locals instead of reloading them through the table on every record.
    pub fn view(&self) -> View<'_> {
        View { table: self, tags: self.tags.as_ptr(), slots: self.slots.as_ptr(), n: self.tags.len() }
    }

    /// The key's id if the published entry `e` holds it; `b` is the mapping.
    #[inline(always)]
    fn same(e: &Slot, k: Key, b: &[u8]) -> Option<u32> {
        let w = |i: usize| e.key[i].load(Ordering::Relaxed);
        let meta = e.meta.load(Ordering::Relaxed);
        let found = meta >> 32 == k.len
            && if k.len <= 24 {
                // Three word compares, folded into one test.
                (w(0) ^ k.w[0]) | (w(1) ^ k.w[1]) | (w(2) ^ k.w[2]) == 0
            } else {
                let (at, mine, n) = (w(0) as usize, k.w[0] as usize, k.w[1] as usize);
                w(1) == k.w[1] && b[at..at + n] == b[mine..mine + n]
            };
        found.then_some(meta as u32)
    }

    /// A bucket on the key's probe sequence with an empty or pending lane: waits out pending
    /// inserts, then finds the key published in it or claims its first empty lane. Returns the id,
    /// or None when the bucket must be read again (the caller loops).
    #[cold]
    #[inline(never)]
    fn settle(&self, word: &AtomicU64, slots: &Slots, k: Key, b: &[u8], tag: u64) -> Option<u32> {
        let t = word.load(Ordering::Acquire);
        let lanes = (0..4).map(|i| t >> (16 * i) & 0xFFFF);
        if lanes.clone().any(|l| l == PENDING) {
            std::hint::spin_loop();
            return None;
        }
        if let Some(id) = lanes.clone().enumerate().filter(|&(_, l)| l == tag).find_map(|(i, _)| Self::same(&slots.0[i], k, b)) {
            return Some(id);
        }
        if self.overflowed() {
            return Some(0);
        }
        // The first empty lane (one exists: the caller saw it, and lanes are only ever filled).
        let lane = lanes.clone().position(|l| l == 0)?;
        if word.compare_exchange(t, t | PENDING << (16 * lane), Ordering::Acquire, Ordering::Relaxed).is_err() {
            return None;
        }
        let e = &slots.0[lane];
        for (slot, w) in e.key.iter().zip(k.w) {
            slot.store(w, Ordering::Relaxed);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if id >= self.limit {
            self.overflow.store(true, Ordering::Relaxed);
        }
        e.meta.store(k.len << 32 | id as u64, Ordering::Relaxed);
        // PENDING (1) + tag - 1 = tag, inside the lane: no carry into its neighbour.
        word.fetch_add((tag - 1) << (16 * lane), Ordering::Release);
        Some(id)
    }
}

/// A table's hot side (`Table::view`): its base pointers and bucket count, copied into the worker.
#[derive(Clone, Copy)]
pub struct View<'a> {
    table: &'a Table,
    tags: *const AtomicU64,
    slots: *const Slots,
    n: usize,
}

impl View<'_> {
    #[inline(always)]
    fn home(self, h: u64) -> usize {
        ((h as u128 * self.n as u128) >> 64) as usize
    }

    /// Fetches the key's home tag word and the first line of its entries into L1 while the rest
    /// of the record is parsed. Only hints; they never fault.
    #[inline(always)]
    pub fn prefetch(self, k: Key) {
        let at = self.home(k.h);
        // SAFETY: `at` is below the bucket count; a prefetch reads nothing architecturally.
        unsafe {
            _mm_prefetch(self.tags.add(at) as *const i8, _MM_HINT_T0);
            _mm_prefetch(self.slots.add(at) as *const i8, _MM_HINT_T0);
        }
    }

    /// The id of the key, inserting it if it is new. Returns 0 without inserting once the table has
    /// overflowed (the pass is rerun).
    #[inline(always)]
    pub fn id(self, k: Key, b: &[u8]) -> u32 {
        let tag = (k.h & 0xFFFF).max(2);
        let (mut at, mut probed) = (self.home(k.h), 0);
        loop {
            // SAFETY: `at` stays below the bucket count, and the table outlives the view.
            let (word, slots) = unsafe { (&*self.tags.add(at), &*self.slots.add(at)) };
            let t = word.load(Ordering::Acquire);
            // Lanes equal to the tag: a zero lane in t ^ tag (SWAR; a lane above a match may show
            // falsely, which only costs that entry's compare).
            let x = t ^ tag * LANES;
            let mut hits = x.wrapping_sub(LANES) & !x & 0x8000 * LANES;
            while hits != 0 {
                if let Some(id) = Table::same(&slots.0[hits.trailing_zeros() as usize / 16], k, b) {
                    return id;
                }
                hits &= hits - 1;
            }
            // An empty or pending lane: the key is absent so far, or being inserted right now.
            let low = t & !LANES; // a lane below 2 (empty or PENDING) is now 0
            if low.wrapping_sub(LANES) & !low & 0x8000 * LANES != 0 {
                if let Some(id) = self.table.settle(word, slots, k, b, tag) {
                    return id;
                }
                continue;
            }
            at = if at + 1 == self.n { 0 } else { at + 1 };
            probed += 1;
            // Every bucket full and none holds the key: only an overflowed table gets here.
            if probed == self.n {
                self.table.overflow.store(true, Ordering::Relaxed);
                return 0;
            }
        }
    }
}

/// A set of name ids, counting its members as they arrive.
#[derive(Clone)]
pub struct NameSet {
    words: Vec<u64>,
    pub len: u64,
}

impl NameSet {
    /// A set for ids below `ids`: a pass's ids stay below its table's entry count, as each id is
    /// handed out for one claimed entry. Sized once, so an insert has no growth check (880ff14's
    /// grew in 1 KiB steps, a length test per insert). Zeroed pages are only charged once written.
    pub fn new(ids: usize) -> NameSet {
        NameSet { words: vec![0; ids.div_ceil(64)], len: 0 }
    }

    /// # Safety
    /// `id` must be below the `ids` the set was made for.
    #[inline(always)]
    pub unsafe fn insert(&mut self, id: u32) {
        let (w, bit) = (unsafe { self.words.get_unchecked_mut(id as usize / 64) }, 1 << (id % 64));
        self.len += (*w & bit == 0) as u64;
        *w |= bit;
    }
}
