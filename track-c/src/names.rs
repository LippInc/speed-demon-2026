//! Distinct names: one intern table shared by every worker gives each distinct name a dense id
//! (0 upward), and each group keeps a bitset over those ids (pass2.rs), so names_distinct is the
//! bitset's popcount.

use std::cell::UnsafeCell;
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering};

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

/// One slot. `meta` is 0 while empty, else `(low 32 hash bits) << 32 | (id + 1)`; `name` points at
/// the name in its shard's arena: a little-endian u32 length, then the raw bytes. The stored hash
/// bits pick the home slot again when the table doubles, and filter probes before the byte compare.
struct Slot {
    meta: AtomicU64,
    name: AtomicPtr<u8>,
}

/// One generation of a shard's slots (a power of two of them).
struct Slots {
    mask: usize,
    slots: Box<[Slot]>,
}

impl Slots {
    fn new(len: usize) -> Box<Slots> {
        let slots = (0..len).map(|_| Slot { meta: AtomicU64::new(0), name: AtomicPtr::new(ptr::null_mut()) }).collect();
        Box::new(Slots { mask: len - 1, slots })
    }

    /// Probes for `key` from its home slot. Returns its id, or the index of the empty slot that
    /// ends the probe. Every slot read here was published (name, then meta with Release) before
    /// its meta is seen non-zero (Acquire), so the arena bytes it points at are complete.
    #[inline]
    fn find(&self, key: &[u8], tag: u32) -> Result<u32, usize> {
        let mut pos = tag as usize & self.mask;
        loop {
            let slot = &self.slots[pos];
            let meta = slot.meta.load(Ordering::Acquire);
            if meta == 0 {
                return Err(pos);
            }
            if (meta >> 32) as u32 == tag {
                // SAFETY: a published name points into an arena chunk that lives, unmoved, as long as
                // the Intern does (chunks are only ever appended), and holds a length then that many bytes.
                let stored = unsafe {
                    let p = slot.name.load(Ordering::Relaxed);
                    let len = u32::from_le_bytes(*(p as *const [u8; 4])) as usize;
                    std::slice::from_raw_parts(p.add(4), len)
                };
                if stored == key {
                    return Ok(meta as u32 - 1);
                }
            }
            pos = (pos + 1) & self.mask;
        }
    }
}

/// What only the inserting thread touches, under the shard's lock.
struct Writer {
    /// Every generation of slots, the current one last. Older ones are kept until the Intern drops,
    /// because a lock-free reader may still be probing one.
    generations: Vec<Box<Slots>>,
    names: usize,
    /// Append-only arena chunks: each distinct name's bytes, copied once on insert, so a hit compares
    /// against them instead of the name's first occurrence in the ~200 MB mapping (c-join.md:196), a
    /// random and almost always uncached read. On the c-4 harness (12 workers, this VM) parse + intern
    /// fell from 541 to 410 ms, and on the box T from 0.159 to 0.150 s (c-5, 60e701c), for about 1.5 MB.
    /// A chunk is never resized, so the names in it never move. Its bytes are `UnsafeCell`s because
    /// readers hold pointers into a chunk while the writer appends behind them: the writer only ever
    /// writes bytes no published name covers, through raw pointers, never through a `&mut` chunk.
    chunks: Vec<Box<[UnsafeCell<u8>]>>,
    used: usize,
}

/// Arena chunks start at 4 KiB and double up to 64 KiB, so a shard's arena grows with what it holds.
const CHUNK_MIN: usize = 4 << 10;
const CHUNK_MAX: usize = 64 << 10;

impl Writer {
    /// Copies `key` into the arena behind its length; returns where it starts.
    fn store(&mut self, key: &[u8]) -> *mut u8 {
        let need = 4 + key.len();
        if self.chunks.last().is_none_or(|c| c.len() - self.used < need) {
            let next = self.chunks.last().map_or(CHUNK_MIN, |c| (c.len() * 2).min(CHUNK_MAX));
            self.chunks.push((0..next.max(need)).map(|_| UnsafeCell::new(0)).collect());
            self.used = 0;
        }
        let chunk = self.chunks.last().unwrap();
        // SAFETY: `used..used + need` lies inside the chunk and no published name covers it yet, so
        // nothing reads these bytes while they are written.
        unsafe {
            let at = UnsafeCell::raw_get(chunk.as_ptr().add(self.used));
            ptr::copy_nonoverlapping((key.len() as u32).to_le_bytes().as_ptr(), at, 4);
            ptr::copy_nonoverlapping(key.as_ptr(), at.add(4), key.len());
            self.used += need;
            at
        }
    }

    /// Doubles the slots: copies every published slot into a new generation (only this thread
    /// writes slots, so plain loads suffice) and publishes it; readers still on the old generation
    /// find every name it held, and a miss there sends them to the locked re-check.
    fn grow(&mut self, current: &AtomicPtr<Slots>) {
        let old = self.generations.last().unwrap();
        let new = Slots::new(old.slots.len() * 2);
        for slot in old.slots.iter() {
            let meta = slot.meta.load(Ordering::Relaxed);
            if meta != 0 {
                let mut pos = (meta >> 32) as usize & new.mask;
                while new.slots[pos].meta.load(Ordering::Relaxed) != 0 {
                    pos = (pos + 1) & new.mask;
                }
                new.slots[pos].name.store(slot.name.load(Ordering::Relaxed), Ordering::Relaxed);
                new.slots[pos].meta.store(meta, Ordering::Relaxed);
            }
        }
        // Release: a reader that loads this pointer (Acquire) sees every slot copied above.
        current.store(&*new as *const Slots as *mut Slots, Ordering::Release);
        self.generations.push(new);
    }
}

/// Shards are picked by the hash's top bits, so 12 workers rarely wait on the same lock.
const SHARD_BITS: u32 = 8;

/// One shard on its own cache line. Hits read `current` and the slots without locking; the mutex
/// is taken only to insert. Hits are about 97 % of lookups (100.5k distinct names in 3.32M records,
/// seed 1), so the lock and its cache-line traffic leave the common path.
#[repr(align(64))]
struct Shard {
    current: AtomicPtr<Slots>,
    writer: Mutex<Writer>,
}

/// Raw name bytes (between the quotes, escapes kept) -> dense id. `""` -> `"` is one to one, so raw
/// bytes are distinct exactly when the unescaped values are; `""` and an empty unquoted field are
/// both the empty key. Every hit is an exact byte compare against the copy in the shard's arena.
pub struct Intern {
    shards: Vec<Shard>,
    next_id: AtomicU32,
}

impl Intern {
    /// `slots_per_shard` is each shard's starting size (a power of two, grown by doubling at half load).
    pub fn new(slots_per_shard: usize) -> Intern {
        let len = slots_per_shard.next_power_of_two().max(2);
        let shards = (0..1 << SHARD_BITS)
            .map(|_| {
                let first = Slots::new(len);
                Shard {
                    current: AtomicPtr::new(&*first as *const Slots as *mut Slots),
                    writer: Mutex::new(Writer { generations: vec![first], names: 0, chunks: Vec::new(), used: 0 }),
                }
            })
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
        let shard = &self.shards[(h >> (64 - SHARD_BITS)) as usize];
        // SAFETY: `current` always points at a generation owned by the shard's writer, and no
        // generation is freed before the Intern drops.
        let slots = unsafe { &*shard.current.load(Ordering::Acquire) };
        if let Ok(id) = slots.find(key, tag) {
            return id;
        }
        // Miss: take the lock, re-check the current generation (another worker may have inserted
        // it, or grown the table), then insert and publish.
        let mut w = shard.writer.lock().unwrap();
        let mut pos = match w.generations.last().unwrap().find(key, tag) {
            Ok(id) => return id,
            Err(pos) => pos,
        };
        // Grow before this insert would fill the generation past half. Every generation a reader can
        // reach then keeps an empty slot, which is what ends a lock-free probe for a name it lacks: a
        // full retired generation (2 slots, both used) made such a probe spin forever, found by the
        // stress runs at SD_INTERN_SLOTS=2.
        if (w.names + 1) * 2 > w.generations.last().unwrap().slots.len() {
            w.grow(&shard.current);
            pos = w.generations.last().unwrap().find(key, tag).expect_err("name absent before the grow");
        }
        // New name: its id is taken under the shard lock, so ids stay dense across shards.
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        assert!(id < u32::MAX, "more than 2^32 - 1 distinct names");
        let name = w.store(key);
        let slot = &w.generations.last().unwrap().slots[pos];
        slot.name.store(name, Ordering::Relaxed);
        // Release: a reader that sees this meta (Acquire) sees the name pointer and its bytes.
        slot.meta.store((tag as u64) << 32 | (id as u64 + 1), Ordering::Release);
        w.names += 1;
        id
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
