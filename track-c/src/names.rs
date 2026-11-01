//! Distinct names, design D-B (c-52): each record's name goes, as a message, to the worker that owns
//! it (by a hash of the name), and that owner alone keeps the name in its private tables, each name
//! with the set of groups that have seen it. Nothing is shared per name: no compare-and-swap, no
//! global id. names_distinct of a group is the sum over owners of their new (name, group) bits.
//!
//! A name is keyed by its raw bytes between the quotes, escapes kept: `""` -> `"` is one to one and an
//! unquoted value holds no `"` (c-join.md:27-28), so raw keys are equal exactly when the values are, and
//! the empty unquoted name and `""` are both the empty key (c-join.md:34).
//!
//! One key kind: 56 bits of a length-seeded multiply hash of the raw bytes, one lane of c-81's
//! two-lane hash. Two distinct names collide only if their 56 bits match: N^2 / 2^57 with
//! N = 100,000 (c-join.md 2.2's planning pool) is about 7e-8 per run (birthday bound); the empty
//! name hashes like any other (its length 0 seeds it).

use std::arch::x86_64::*;

use crate::records::load;

/// One 8-byte slot of an owner's inbox: the name's 56-bit key with the group in the top byte
/// (groups are at most 256, regions.rs).
pub type Msg = u64;

/// A message's low 56 bits: its name's key.
const LOW56: u64 = (1 << 56) - 1;

/// One round of a 128-bit folded multiply: xor of the product's halves mixes both.
#[inline(always)]
fn mum(a: u64, b: u64) -> u64 {
    let m = a as u128 * b as u128;
    m as u64 ^ (m >> 64) as u64
}

/// A key's 32-bit hash: crc32c of its word, one instruction. The sender picks the owner from its
/// high bits, the owner a bucket from its low bits.
// SAFETY: SSE4.2 is on at compile time (x86-64-v3).
#[inline(always)]
fn crc(w: u64) -> u32 { unsafe { _mm_crc32_u64(0, w) as u32 } }

/// The name of one record, made into its owner's message while its bytes are still in L1 (c-51:
/// packing at the sender measured 23.58 ms of pass against 27.3 when the owner read the mapping).
pub struct Name(pub u32, pub Msg);

impl Name {
    /// The name `p[at..at + len]` of a record in group `g`, as its owner's message: the name's
    /// 56-bit key with the group in the top byte, and the key's hash.
    ///
    /// # Safety
    /// The name and 32 bytes from its start must lie inside the padded mapping (GUARD), and so must
    /// the byte before it (the opening quote, or the comma before an unquoted name).
    #[inline(always)]
    pub unsafe fn new(p: *const u8, at: usize, len: usize, g: usize) -> Name {
        let mut a = 0xa076_1d64_78bd_642f ^ len as u64;
        for k in 0..len.div_ceil(8) {
            // The name's own bytes are followed by at least `",` and more fields, and GUARD covers the
            // end of the file, so every load is in bounds; the last word is masked to the bytes left.
            let w = unsafe { _bzhi_u64(load::<u64>(p, at + 8 * k), (8 * (len - 8 * k)).min(64) as u32) };
            a = mum(a ^ w, 0x8ebc_6af0_9c88_c6e3);
        }
        let h = mum(a, 0x1d8e_4e27_c47d_124f) & LOW56;
        Name(crc(h), h | (g as u64) << 56)
    }
}

/// One cache line of a table's index: 16 entries `1 << 31 | tag | id`, filled in order. Bit 31 set =
/// in use; bits `idb..=30` = the key hash's tag bits; bits `0..idb` = the record id. The split is
/// `Table`'s `idb`, set per index size: the tag takes every bit the ids do not need, and it wants
/// them — a false tag hit costs a key read from `recs`, a likely cache miss (7 tag bits cost
/// +3.2 % T in c-84 s1, deb7823).
#[repr(align(64))] struct Bucket([u32; 16]);

/// One owner's names: an index of 16-entry buckets over a dense array of records
/// `(key, groups)`, bit g of the groups set once group g has seen the name (256 bits, regions.rs's
/// limit). Lookups start at the hash's home bucket and walk on to the next only past a full one
/// (all 16 entries in use); entries are never moved or deleted, so a key that is there sits before
/// the first free entry of its walk. Each entry splits its 32 bits at run time as `idb` id bits
/// below the tag: the index grows once ids reach `12 * len` (3/4 of 16 entries), so `idb` sizes to
/// the bits of `12 * len - 1` and the tag gets every bit left above it — a wider tag means fewer
/// false tag hits, and each one costs a key read from `recs` (7 bits cost +3.2 % T in c-84 s1,
/// deb7823). Tag bits stay >= 1 while `12 * len <= 2^30`, far past any run (a full-size file has
/// 3,320,000 records, c-join.md 2.4). The index starts at 2.2's planning pool (100,000 names over
/// the workers, 12 entries per bucket at the load limit, pass2) and doubles past 3/4 full,
/// rehashing from the keys (the tag no longer holds the hash's home bits); a record keeps its id,
/// so no input size or count ever sizes it (rule 31); grow is the fallback.
struct Table { index: Box<[Bucket]>, recs: Vec<(u64, [u64; 4])>, idb: u32 }

impl Table {
    /// A zeroed index of `n` buckets: every entry free.
    fn zeros(n: usize) -> Box<[Bucket]> {
        // SAFETY: an all-zero bucket is valid: every entry free.
        unsafe { Box::<[Bucket]>::new_zeroed_slice(n).assume_init() }
    }

    fn new(buckets: usize) -> Self {
        // Sized to the first index's capacity (3/4 of 16 entries per bucket) so the records never double
        // and copy at full size (2.2's planning pool, via pass2's sizing); a larger input still grows both.
        // Ids stay below 12 * buckets, so the id field takes the bits of 12 * buckets - 1.
        let idb = (12 * buckets - 1).ilog2() + 1;
        Table { index: Self::zeros(buckets), recs: Vec::with_capacity(12 * buckets), idb }
    }

    /// The tag bits of an entry: the hash above the id field (bit 31, the in-use flag, excepted).
    #[inline(always)]
    fn tag_mask(&self) -> u32 { 0x7FFF_FFFF & !((1 << self.idb) - 1) }

    /// A hash's home bucket: `log2 len` low bits of `h`, which sit below the id field (`log2 len <
    /// idb`, sized to `12 * len`), so tag and home bucket use different bits of `h`.
    #[inline(always)]
    fn home(&self, h: u32) -> usize { h as usize & (self.index.len() - 1) }

    /// Fetches the hash's home bucket into L1 ahead of its lookup. Only a hint; it never faults.
    #[inline(always)]
    fn prefetch(&self, h: u32) {
        // SAFETY: the home bucket is inside the index; a prefetch reads nothing architecturally.
        unsafe { _mm_prefetch::<_MM_HINT_T0>(self.index.as_ptr().add(self.home(h)) as *const i8) };
    }

    /// The groups of the key `key` with hash `h`, inserting it if it is not there.
    #[inline(always)]
    fn groups(&mut self, h: u32, key: u64) -> &mut [u64; 4] {
        let (tag, mask, mut at) = (1 << 31 | h & self.tag_mask(), (1 << self.idb) - 1, self.home(h));
        // SAFETY: AVX2 is on at compile time; `at` stays inside the index; each id in an entry indexes
        // `recs` (it was its length when the entry went in) and fits the id field (insert grows first).
        unsafe {
            let tags = _mm256_set1_epi32(tag as i32);
            let keep = _mm256_set1_epi32((1 << 31 | self.tag_mask()) as i32);
            loop {
                let line = self.index.as_ptr().add(at) as *const __m256i;
                let (lo, hi) = (_mm256_load_si256(line), _mm256_load_si256(line.add(1)));
                // One bit per entry: the entry's tag bits equal the key's, the in-use bit included (a
                // free entry is all zero and never matches a tag, whose bit 31 is set).
                let eq = |v| {
                    _mm256_movemask_ps(_mm256_castsi256_ps(_mm256_cmpeq_epi32(
                        _mm256_and_si256(v, keep),
                        tags,
                    ))) as u32
                };
                let mut hits = eq(lo) | eq(hi) << 8;
                while hits != 0 {
                    let id = (self.index[at].0[hits.trailing_zeros() as usize] & mask) as usize;
                    if self.recs.get_unchecked(id).0 == key {
                        return &mut self.recs.get_unchecked_mut(id).1;
                    }
                    hits &= hits - 1;
                }
                // Each entry's sign bit: in use exactly when set, so 0xFFFF is a full line.
                let used = _mm256_movemask_ps(_mm256_castsi256_ps(lo)) as u32
                    | (_mm256_movemask_ps(_mm256_castsi256_ps(hi)) as u32) << 8;
                if used != 0xFFFF {
                    return self.insert(at, used.count_ones() as usize, h, key);
                }
                at = (at + 1) & (self.index.len() - 1);
            }
        }
    }

    /// Files a new key in entry `lane` of bucket `at` (its first free one), growing first past 3/4.
    #[cold]
    #[inline(never)]
    fn insert(&mut self, mut at: usize, mut lane: usize, h: u32, key: u64) -> &mut [u64; 4] {
        let id = self.recs.len();
        if 4 * (id + 1) > 3 * 16 * self.index.len() {
            self.grow();
            (at, lane) = self.free(h);
        }
        self.index[at].0[lane] = 1 << 31 | h & self.tag_mask() | id as u32;
        self.recs.push((key, [0; 4]));
        &mut self.recs[id].1
    }

    /// The first free entry on a hash's walk, as (bucket, lane).
    fn free(&self, h: u32) -> (usize, usize) {
        let mut at = self.home(h);
        loop {
            if let Some(lane) = self.index[at].0.iter().position(|&e| e == 0) { return (at, lane); }
            at = (at + 1) & (self.index.len() - 1);
        }
    }

    /// Doubles the index and refiles every entry, recomputing its hash from the key: the tag's bits
    /// above the id field move as the ids widen, and the tag alone no longer knows the home bucket.
    fn grow(&mut self) {
        let bigger = Self::zeros(2 * self.index.len());
        let old = std::mem::replace(&mut self.index, bigger);
        let mask = (1 << self.idb) - 1;
        self.idb += 1;
        for &e in old.iter().flat_map(|b| &b.0).filter(|&&e| e != 0) {
            let h = crc(self.recs[(e & mask) as usize].0);
            let (at, lane) = self.free(h);
            self.index[at].0[lane] = 1 << 31 | h & self.tag_mask() | e & mask;
        }
    }
}

/// One worker's side as an owner: its names, and per group how many of them it has seen in that
/// group.
pub struct Owner { table: Table, pub counts: [u64; 256] }

impl Owner {
    pub fn new(b: usize) -> Self { Owner { table: Table::new(b), counts: [0; 256] } }

    /// Files a bucket of messages. Each name's home bucket is prefetched 16 messages ahead (c-50
    /// b6's distance).
    pub fn file(&mut self, msgs: &[Msg]) {
        for (j, &m) in msgs.iter().enumerate() {
            if let Some(&a) = msgs.get(j + 16) { self.table.prefetch(crc(a & LOW56)); }
            // The group is the message's top byte; the key, its low 56 bits.
            let (g, k) = ((m >> 56) as usize, m & LOW56);
            let groups = self.table.groups(crc(k), k);
            let (word, bit) = (&mut groups[g / 64], 1 << (g % 64));
            self.counts[g] += (*word & bit == 0) as u64;
            *word |= bit;
        }
    }
}
