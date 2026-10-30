//! Distinct names: every name gets a dense id from one exact-compare intern table, and each group
//! keeps a bitset over those ids, so names_distinct is the bitset's popcount.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// FxHash (rustc's hasher): one rotate, xor and multiply per 8 bytes. The table compares keys
/// exactly, so the hash only has to spread names, not resist an adversary.
#[derive(Default)]
pub struct FxHasher(u64);

const FX_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl FxHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(FX_SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            self.add(u64::from_le_bytes(c.try_into().unwrap()));
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            let mut tail = [0u8; 8];
            tail[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(tail));
        }
    }

    #[inline]
    fn write_usize(&mut self, n: usize) {
        self.add(n as u64);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// Name key -> dense id. The key is the raw bytes between the quotes, still escaped: `""` -> `"`
/// is one to one, so raw bytes are distinct exactly when the unescaped values are, and the keys
/// stay zero-copy slices of the mapped file. `""` and an empty unquoted field are both the empty key.
#[derive(Default)]
pub struct NameIds(HashMap<&'static [u8], u32, BuildHasherDefault<FxHasher>>);

impl NameIds {
    #[inline]
    pub fn id(&mut self, raw: &'static [u8]) -> u32 {
        let next = self.0.len() as u32;
        *self.0.entry(raw).or_insert(next)
    }
}

/// A set of name ids, grown on demand.
#[derive(Default)]
pub struct NameSet(Vec<u64>);

/// Growth step in words (1 KiB): few reallocations, and at most 1 KiB of slack per group.
const GROW_WORDS: usize = 128;

impl NameSet {
    #[inline]
    pub fn insert(&mut self, id: u32) {
        let word = (id / 64) as usize;
        if word >= self.0.len() {
            let len = (word + 1).next_multiple_of(GROW_WORDS);
            self.0.reserve_exact(len - self.0.len());
            self.0.resize(len, 0);
        }
        self.0[word] |= 1 << (id % 64);
    }

    pub fn len(&self) -> u64 {
        self.0.iter().map(|w| w.count_ones() as u64).sum()
    }
}
