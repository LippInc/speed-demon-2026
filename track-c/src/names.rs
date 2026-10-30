//! Distinct names: one intern table shared by every worker gives each distinct name a dense id
//! (0 upward), and each group keeps a bitset over those ids (pass2.rs), so names_distinct is the
//! bitset's popcount.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

/// FxHash (rustc's hasher: rotate, xor, multiply per 8 bytes) with the length mixed in first, so
/// zero-padding the tail cannot make two names agree, and murmur3's fmix64 at the end, because the
/// table indexes by the low bits and Fx's multiply leaves those weak.
fn hash(raw: &[u8]) -> u64 {
    const FX_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;
    let add = |h: u64, word: u64| (h.rotate_left(5) ^ word).wrapping_mul(FX_SEED);
    let mut h = add(0, raw.len() as u64);
    let mut chunks = raw.chunks_exact(8);
    for c in &mut chunks {
        h = add(h, u64::from_le_bytes(c.try_into().unwrap()));
    }
    let rest = chunks.remainder();
    if !rest.is_empty() {
        let mut tail = [0u8; 8];
        tail[..rest.len()].copy_from_slice(rest);
        h = add(h, u64::from_le_bytes(tail));
    }
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^ (h >> 33)
}

/// A name the table holds: where its raw bytes are in the mapping (never copied) and its id.
struct Entry {
    offset: u32,
    len: u32,
    id: u32,
}

/// One shard: open addressing with linear probing. A slot is 0 when empty, else
/// `(low 32 hash bits) << 32 | (entry index + 1)`; the stored hash bits pick the home slot again
/// when the table doubles, and filter probes before the byte compare.
struct Table {
    slots: Vec<u64>,
    entries: Vec<Entry>,
}

/// Shards are picked by the hash's top bits, so 12 workers rarely wait on the same lock.
const SHARD_BITS: u32 = 8;

/// Each shard on its own cache line, so two workers' locks never share one.
#[repr(align(64))]
struct Shard(Mutex<Table>);

/// Raw name bytes (between the quotes, escapes kept) -> dense id. `""` -> `"` is one to one, so raw
/// bytes are distinct exactly when the unescaped values are; `""` and an empty unquoted field are
/// both the empty key. Every hit is an exact byte compare against the mapping.
pub struct Intern {
    shards: Vec<Shard>,
    next_id: AtomicU32,
}

impl Intern {
    /// `slots_per_shard` is each shard's starting size (a power of two, grown by doubling at half load).
    pub fn new(slots_per_shard: usize) -> Intern {
        let slots = slots_per_shard.next_power_of_two().max(2);
        let shards = (0..1 << SHARD_BITS)
            .map(|_| Shard(Mutex::new(Table { slots: vec![0; slots], entries: Vec::new() })))
            .collect();
        Intern { shards, next_id: AtomicU32::new(0) }
    }

    /// Distinct names interned so far; ids are 0..count.
    pub fn count(&self) -> u32 {
        self.next_id.load(Ordering::Relaxed)
    }

    /// The id of `b[start..start + len]`, inserting it if it is new.
    pub fn id(&self, b: &[u8], start: usize, len: usize) -> u32 {
        let key = &b[start..start + len];
        let h = hash(key);
        let tag = h as u32;
        let mut t = self.shards[(h >> (64 - SHARD_BITS)) as usize].0.lock().unwrap();
        let mask = t.slots.len() - 1;
        let mut pos = tag as usize & mask;
        loop {
            let slot = t.slots[pos];
            if slot == 0 {
                break;
            }
            if (slot >> 32) as u32 == tag {
                let e = &t.entries[(slot as u32 - 1) as usize];
                if &b[e.offset as usize..(e.offset + e.len) as usize] == key {
                    return e.id;
                }
            }
            pos = (pos + 1) & mask;
        }
        // New name: its id is taken under the shard lock, so ids stay dense across shards.
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        t.entries.push(Entry { offset: start as u32, len: len as u32, id });
        t.slots[pos] = (tag as u64) << 32 | t.entries.len() as u64;
        if t.entries.len() * 2 > t.slots.len() {
            t.grow();
        }
        id
    }
}

impl Table {
    fn grow(&mut self) {
        let mut slots = vec![0u64; self.slots.len() * 2];
        let mask = slots.len() - 1;
        for &slot in self.slots.iter().filter(|&&s| s != 0) {
            let mut pos = (slot >> 32) as usize & mask;
            while slots[pos] != 0 {
                pos = (pos + 1) & mask;
            }
            slots[pos] = slot;
        }
        self.slots = slots;
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
