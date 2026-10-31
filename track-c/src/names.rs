//! Distinct names, design D-B (c-52): each record's name goes, as a message, to the worker that owns
//! it (by a hash of the name), and that owner alone keeps the name in its private tables, each name
//! with the set of groups that have seen it. Nothing is shared per name: no compare-and-swap, no
//! global id. names_distinct of a group is the sum over owners of their new (name, group) bits.
//!
//! A name is keyed by its raw bytes between the quotes, escapes kept: `""` -> `"` is one to one and an
//! unquoted value holds no `"` (c-join.md:27-28), so raw keys are equal exactly when the values are, and
//! the empty unquoted name and `""` are both the empty key (c-join.md:34).
//!
//! Three kinds of key, by the name's raw bytes:
//! - short: up to 16 bytes, all below 0x80, the last one not 0: the two zero-padded words. Zero padding
//!   is one to one once the last byte is not 0, and every bit 7 is free (89 % of seed 1's records).
//! - medium: 17 to 32 such bytes: four words, sent as two messages (11 %; 99.99 % of names fit 24).
//! - exact: anything else (longer, a byte of 0x80 or more, a trailing 0; c-join.md 2.1 allows them,
//!   2.2 never writes them): the name's offset and length, and the owner keeps the bytes it reads
//!   from the mapping in a plain HashMap, off the fast path.

use std::arch::x86_64::*;
use std::collections::HashMap;
use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};

use crate::records::load;

/// One 16-byte slot of an owner's inbox. A short or medium name's first slot is its first two key
/// words with the group in word 0's eight bit-7 positions and the kind in word 1's; a medium name's
/// second slot is its last two words, marked TAIL; an exact name's is its offset with the group in
/// the top byte, then its length marked EXACT. The kind marks sit in bit-7 positions, which no key
/// byte uses, so they never collide with key bits.
pub type Msg = [u64; 2];

/// Bit 7 of every byte.
const HIGH: u64 = 0x8080_8080_8080_8080;
/// Kind marks in a slot's word 1: bit 7 of bytes 14 and 15.
const MEDIUM: u64 = 1 << 55;
const EXACT: u64 = 1 << 63;
const TAIL: u64 = MEDIUM | EXACT;

/// The key's word `k` (bytes 8k..8k+8) with the bytes past the key cleared: BMI2's bzhi keeps the
/// low `8 * len - 64 * k` bits (all 64 from a count of 64 to 255, none at 0), so no branch on the
/// length. bzhi reads only the count's low byte, so a word is right while the count is under 256:
/// the short path uses its words only up to 16 bytes.
///
/// # Safety
/// The 8 bytes at `p + at + 8k` must lie inside the padded mapping.
#[inline(always)]
unsafe fn key_word(p: *const u8, at: usize, len: usize, k: usize) -> u64 {
    let bits = (8 * len).saturating_sub(64 * k) as u32;
    unsafe { _bzhi_u64(load::<u64>(p, at + 8 * k), bits) }
}

/// An exact name's hash, used at the sender; the owner's HashMap hashes the bytes itself. A fixed
/// hasher: sender and owner of one name must agree, whichever thread asks.
#[cold]
#[inline(never)]
fn exact_hash(key: &[u8]) -> u32 {
    (BuildHasherDefault::<DefaultHasher>::default().hash_one(key) >> 32) as u32
}

/// A short or medium key's 32-bit hash: crc32c of its words, one instruction each. The sender picks
/// the owner from its high bits, the owner a bucket from its low bits.
#[inline(always)]
fn crc(words: &[u64]) -> u32 {
    // SAFETY: SSE4.2 is on at compile time (x86-64-v3).
    words.iter().fold(0, |h, &w| unsafe { _mm_crc32_u64(h, w) }) as u32
}

/// The name of one record, made into its owner's messages while its bytes are still in L1 (c-51:
/// packing at the sender measured 23.58 ms of pass against 27.3 when the owner read the mapping).
pub enum Name {
    One(u32, Msg),
    Two(u32, Msg, Msg),
}

impl Name {
    /// The name `p[at..at + len]` of a record in group `g`, with its key's hash.
    ///
    /// # Safety
    /// The name and 32 bytes from its start must lie inside the padded mapping (GUARD), and so must
    /// the byte before it (the opening quote, or the comma before an unquoted name).
    #[inline(always)]
    pub unsafe fn new(p: *const u8, at: usize, len: usize, g: usize) -> Name {
        let (w0, w1) = unsafe { (key_word(p, at, len, 0), key_word(p, at, len, 1)) };
        // The last byte is not 0 (for the empty name it reads the quote or comma before it).
        let last = unsafe { *p.add(at + len - 1) } != 0;
        if len <= 16 && (w0 | w1) & HIGH == 0 && last {
            // The group in word 0's bit-7 positions: groups are at most 256 (regions.rs).
            let m = [w0 | unsafe { _pdep_u64(g as u64, HIGH) }, w1];
            return Name::One(crc(&[w0, w1]), m);
        }
        unsafe { Name::long(p, at, len, last, g) }
    }

    /// A medium or exact name: off the short path, which takes 89 % of the records.
    #[inline(never)]
    unsafe fn long(p: *const u8, at: usize, len: usize, last: bool, g: usize) -> Name {
        // key_word's count, clamped: at 32 bytes word 0's would be 256, which bzhi reads as 0.
        let bits = |k: usize| (8 * len).saturating_sub(64 * k).min(64) as u32;
        let w = [0, 1, 2, 3].map(|k| unsafe { _bzhi_u64(load::<u64>(p, at + 8 * k), bits(k)) });
        if len <= 32 && (w[0] | w[1] | w[2] | w[3]) & HIGH == 0 && last {
            let (w1, w3) = (w[1] | MEDIUM, w[3] | TAIL);
            let m = [w[0] | unsafe { _pdep_u64(g as u64, HIGH) }, w1];
            return Name::Two(crc(&[w[0], w1, w[2], w3]), m, [w[2], w3]);
        }
        // An exact name's group goes in its offset's top byte: offsets stay under 2^47 (x86-64's
        // user address space bounds every mapping), lengths under 2^63.
        let key = unsafe { std::slice::from_raw_parts(p.add(at), len) };
        let m = [at as u64 | (g as u64) << 56, len as u64 | EXACT];
        Name::One(exact_hash(key), m)
    }
}

/// One cache line of a table's index: eight entries `tag << 32 | id`, filled in order. A tag is the
/// key's hash with bit 31 set, so an entry is in use exactly when its sign bit is set.
#[derive(Clone, Copy)]
#[repr(align(64))]
struct Bucket([u64; 8]);

/// One owner's names of one kind of key: an index of 8-entry buckets over a dense array of records
/// `(key, groups)`, bit g of the groups set once group g has seen the name (256 bits, regions.rs's
/// limit). Lookups start at the hash's home bucket and walk on to the next only past a full one;
/// entries are never moved or deleted, so a key that is there sits before the first free entry of
/// its walk. The index starts at one bucket and doubles past 3/4 full, rehashing from the tags
/// alone; a record keeps its id, so no input size or count ever sizes it (rule 31), and every run
/// grows it.
struct Table<K> {
    index: Box<[Bucket]>,
    recs: Vec<(K, [u64; 4])>,
}

impl<K: Copy + PartialEq> Table<K> {
    fn new() -> Self {
        let index: Box<[Bucket]> = Box::new([Bucket([0; 8])]);
        let recs = Vec::new();
        Table { index, recs }
    }

    #[inline(always)]
    fn home(&self, h: u32) -> usize {
        h as usize & (self.index.len() - 1)
    }

    /// Fetches the hash's home bucket into L1 ahead of its lookup. Only a hint; it never faults.
    #[inline(always)]
    fn prefetch(&self, h: u32) {
        // SAFETY: the home bucket is inside the index; a prefetch reads nothing architecturally.
        unsafe { _mm_prefetch::<_MM_HINT_T0>(self.index.as_ptr().add(self.home(h)) as *const i8) };
    }

    /// The groups of the key `key` with hash `h`, inserting it if it is not there.
    #[inline(always)]
    fn groups(&mut self, h: u32, key: K) -> &mut [u64; 4] {
        let tag = h | 1 << 31;
        let mut at = self.home(h);
        // SAFETY: AVX2 is on at compile time; `at` stays inside the index; each id in an entry indexes
        // `recs` (it was its length when the entry went in).
        unsafe {
            let tags = _mm256_set1_epi32(tag as i32);
            loop {
                let line = self.index.as_ptr().add(at) as *const __m256i;
                let (lo, hi) = (_mm256_load_si256(line), _mm256_load_si256(line.add(1)));
                // One bit per 32-bit half; an entry's tag is its upper half, the odd bits.
                let eq =
                    |v| _mm256_movemask_ps(_mm256_castsi256_ps(_mm256_cmpeq_epi32(v, tags))) as u32;
                let mut hits = (eq(lo) | eq(hi) << 8) & 0xAAAA;
                while hits != 0 {
                    let id = self.index[at].0[hits.trailing_zeros() as usize / 2] as u32 as usize;
                    if self.recs.get_unchecked(id).0 == key {
                        return &mut self.recs.get_unchecked_mut(id).1;
                    }
                    hits &= hits - 1;
                }
                let used = _mm256_movemask_pd(_mm256_castsi256_pd(lo))
                    | _mm256_movemask_pd(_mm256_castsi256_pd(hi)) << 4;
                if used != 0xFF {
                    return self.insert(at, used.count_ones() as usize, tag, key);
                }
                at = (at + 1) & (self.index.len() - 1);
            }
        }
    }

    /// Files a new key in entry `lane` of bucket `at` (its first free one), growing first past 3/4.
    #[cold]
    #[inline(never)]
    fn insert(&mut self, mut at: usize, mut lane: usize, tag: u32, key: K) -> &mut [u64; 4] {
        let id = self.recs.len();
        if 4 * (id + 1) > 3 * 8 * self.index.len() {
            self.grow();
            (at, lane) = self.free(tag);
        }
        self.index[at].0[lane] = (tag as u64) << 32 | id as u64;
        self.recs.push((key, [0; 4]));
        &mut self.recs[id].1
    }

    /// The first free entry on a tag's walk, as (bucket, lane).
    fn free(&self, tag: u32) -> (usize, usize) {
        let mut at = self.home(tag);
        loop {
            if let Some(lane) = self.index[at].0.iter().position(|&e| e == 0) {
                return (at, lane);
            }
            at = (at + 1) & (self.index.len() - 1);
        }
    }

    /// Doubles the index and refiles every entry by its tag, whose low bits are the hash's.
    fn grow(&mut self) {
        // SAFETY: an all-zero bucket is valid: every entry free.
        let bigger = unsafe { Box::<[Bucket]>::new_zeroed_slice(2 * self.index.len()).assume_init() };
        let old = std::mem::replace(&mut self.index, bigger);
        for &e in old.iter().flat_map(|b| &b.0).filter(|&&e| e != 0) {
            let (at, lane) = self.free((e >> 32) as u32);
            self.index[at].0[lane] = e;
        }
    }
}

/// Exact names (see the module comment): their bytes and the groups that have seen them.
type ExactNames = HashMap<Vec<u8>, [u64; 4]>;

/// The groups of an exact name, inserting it if it is not there. Off the fast path: generated
/// data has none.
#[cold]
#[inline(never)]
fn exact_groups<'a>(exact: &'a mut ExactNames, key: &[u8]) -> &'a mut [u64; 4] {
    exact.entry(key.to_vec()).or_default()
}

/// One worker's side as an owner: its names of each kind, and per group how many of them it has
/// seen in that group.
pub struct Owner {
    short: Table<[u64; 2]>,
    medium: Table<[u64; 4]>,
    exact: ExactNames,
    pub counts: [u64; 256],
}

impl Owner {
    pub fn new() -> Self {
        let (short, medium) = (Table::new(), Table::new());
        Owner { short, medium, exact: HashMap::new(), counts: [0; 256] }
    }

    /// Files a bucket of messages (`b` is the file, read only for exact names). Each name's home
    /// bucket is prefetched 16 messages ahead (c-50 b6's distance).
    pub fn file(&mut self, msgs: &[Msg], b: &[u8]) {
        let mut j = 0;
        while j < msgs.len() {
            if let Some(&[f0, f1]) = msgs.get(j + 16) {
                match f1 & TAIL {
                    0 => self.short.prefetch(crc(&[f0 & !HIGH, f1])),
                    MEDIUM => {
                        // SAFETY: a medium name's two slots always travel in one bucket (pass2.rs).
                        let [f2, f3] = unsafe { *msgs.get_unchecked(j + 17) };
                        self.medium.prefetch(crc(&[f0 & !HIGH, f1, f2, f3]));
                    }
                    _ => {}
                }
            }
            let [m0, m1] = msgs[j];
            // SAFETY: BMI2 is on at compile time (x86-64-v3).
            let (key0, g) = if m1 & EXACT == 0 {
                (m0 & !HIGH, unsafe { _pext_u64(m0, HIGH) })
            } else {
                (m0 << 8 >> 8, m0 >> 56)
            };
            let groups = match m1 & TAIL {
                0 => {
                    let k = [key0, m1];
                    self.short.groups(crc(&k), k)
                }
                MEDIUM => {
                    j += 1;
                    // SAFETY: as above.
                    let [m2, m3] = *unsafe { msgs.get_unchecked(j) };
                    let k = [key0, m1, m2, m3];
                    self.medium.groups(crc(&k), k)
                }
                _ => {
                    exact_groups(&mut self.exact, &b[key0 as usize..][..(m1 & !EXACT) as usize])
                }
            };
            let (word, bit) = (&mut groups[g as usize / 64], 1 << (g % 64));
            self.counts[g as usize] += (*word & bit == 0) as u64;
            *word |= bit;
            j += 1;
        }
    }
}
