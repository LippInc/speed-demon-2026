//! Distinct names: one intern table shared by every worker gives each distinct name a dense id
//! (0 upward), and each group keeps a bitset over those ids (pass2.rs), so names_distinct is the
//! bitset's popcount.

use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::{cell::UnsafeCell, mem::MaybeUninit, ptr, sync::Mutex};

/// One little-endian word of the mapping at `at`; bytes past its end read as 0, so nothing is read
/// beyond it.
fn word(b: &[u8], at: usize) -> u64 {
    if let Some(w) = b.get(at..at + 8) {
        return u64::from_le_bytes(w.try_into().unwrap());
    }
    let mut w = [0u8; 8];
    w[..b.len() - at].copy_from_slice(&b[at..]);
    u64::from_le_bytes(w)
}

/// 64 x 64 -> 128-bit multiply, folded to 64 bits (wyhash's mum, c-11).
fn mum(a: u64, b: u64) -> u64 {
    let x = a as u128 * b as u128;
    x as u64 ^ (x >> 64) as u64
}

/// The hash of the name `b[start..start + len]`: its first and last words (one masked word under 8
/// bytes; overlapping words for 8-16 bytes), its length, and its middle words when it is longer, folded
/// with one 128-bit multiply per word (wyhash's constants). It replaces FxHash over every word plus
/// murmur3's fmix64, a chain of dependent multiplies. Measured (this VM, 12 workers, full seed 1,
/// interleaved medians of 9): pass2+names 252 -> 224 ms (c-11); probes per lookup 1.320 -> 1.324 on average,
/// 23 -> 26 at most (seed 7331: 1.322 -> 1.325, 31 -> 23). Returns the hash and the two words, which
/// the record compare reuses (`Arena::matches`).
pub fn hash(b: &[u8], start: usize, len: usize) -> (u64, u64, u64) {
    let lo = word(b, start) & if len >= 8 { !0 } else { (1 << 8 * len) - 1 };
    let hi = if len < 8 { 0 } else { word(b, start + len - 8) };
    let mut acc = 0x8ebc_6af0_9c88_c6e3 ^ len as u64;
    for at in (start + 8..(start + len).saturating_sub(8)).step_by(8) {
        acc = mum(acc ^ word(b, at), 0x8ebc_6af0_9c88_c6e3);
    }
    let sig = mum(lo ^ acc ^ 0xa076_1d64_78bd_642f, hi ^ 0xe703_7ed1_a0b4_28db);
    (sig, lo, hi)
}

/// A slot array (a power of two of 8-byte slots), all zero. A slot is 0 while empty, else `(16 hash bits) << 48 | the record's
/// address`, the record being the name's entry in the arena (`Arena`; x86-64 user addresses fit in 48 bits). 8
/// bytes, where c-6's slot was 16 (meta plus a name pointer): measured (sdrig, this VM) M 19.5 ->
/// 15.6 MiB, T unchanged (c-7). Holding the address rather than an arena offset (c-7 to c-11) spares a hit
/// the offset's block arithmetic and directory load before the record; this VM could not resolve it
/// (interleaved medians of pass2+names: +9 % in one run, -7 % in the next), so the box decides (c-12.1). The hash bits
/// filter probes before the compare; growth finds a name's home slot again by rehashing its record.
fn new_slots(len: usize) -> Box<[AtomicU64]> {
    (0..len).map(|_| AtomicU64::new(0)).collect()
}

/// The word readers load to reach a generation: the slot array's address, and log2 of its
/// length in the top byte (user addresses fit in 48 bits), so a lookup needs no load of the array's header.
fn packed(slots: &[AtomicU64]) -> u64 {
    slots.as_ptr().expose_provenance() as u64 | (slots.len().trailing_zeros() as u64) << 56
}

/// Probes `slots` (a power of two of them) for `key` from its home slot. Returns its id, or the index
/// of the empty slot that ends the probe. A slot is published (record written, then the slot stored
/// with Release) before it is seen non-zero here (Acquire), so the record it names is complete.
fn find(slots: &[AtomicU64], key: (&[u8], u64, u64), h: u64) -> Result<u32, usize> {
    let mask = slots.len() - 1;
    let mut pos = h as usize & mask;
    loop {
        let slot = slots[pos].load(Ordering::Acquire);
        if slot == 0 {
            return Err(pos);
        }
        // SAFETY: a published slot names a complete record in an arena block that lives,
        // unmoved, as long as the Intern does.
        if slot >> 48 == h >> 32 & 0xFFFF && let Some(id) = unsafe { matches(record(slot), key) } {
            return Ok(id);
        }
        pos = (pos + 1) & mask;
    }
}

/// The record a published slot names.
fn record(slot: u64) -> *const u8 {
    std::ptr::with_exposed_provenance(slot as usize & ((1 << 48) - 1))
}

/// The id of the record at `p` if its name is `key` (its bytes, first and last word as
/// `hash` returns them). Lengths first, then for up to 16 bytes the two words against the record's
/// (zero-padded to 8 bytes, so the masked word of a short name compares whole), and a byte compare
/// only for longer names: no length-dependent compare loop for nearly every name. Measured (this
/// VM, 12 workers, full seed 1, interleaved medians): pass2+names 240 -> 234 ms in one A/B of 9,
/// 219 -> 219 ms in a second of 11, so at most a small gain (c-11).
///
/// # Safety
/// `p` must come from a slot published after its record was written (`Writer::store`).
unsafe fn matches(p: *const u8, (key, lo, hi): (&[u8], u64, u64)) -> Option<u32> {
    unsafe {
        let len = u32::from_le_bytes(*(p as *const [u8; 4])) as usize;
        let at = |k: usize| u64::from_le_bytes(*(p.add(8 + k) as *const [u8; 8]));
        let same = len == key.len()
            && match len {
                0..8 => at(0) == lo,
                8..=16 => at(0) == lo && at(len - 8) == hi,
                _ => std::slice::from_raw_parts(p.add(8), len) == key,
            };
        same.then(|| u32::from_le_bytes(*(p.add(4) as *const [u8; 4])))
    }
}

/// Every distinct name, copied once on insert as a record `[len: u32 LE][id: u32 LE][raw bytes]`, so
/// a hit compares against it instead of the name's first occurrence in the ~200 MB mapping
/// (c-join.md:196), a random and almost always uncached read. On the c-4 harness (12 workers, this
/// VM) parse + intern fell from 541 to 410 ms, and on the box T from 0.159 to 0.150 s (c-5, 60e701c).
///
/// Records are addressed by a 32-bit offset into a fixed directory of blocks, block k holding
/// `ARENA_BLOCK << k` bytes, so an offset finds its block by arithmetic and no block ever moves or is
/// resized. A record never straddles two blocks: the tail of a block too short for the next record
/// is skipped. Blocks are allocated on first use and published, like the records, by the slot's
/// Release store.
///
/// One arena serves all shards, so its blocks grow large (the last one about half the names' bytes),
/// and large blocks are mapped lazily: allocated uninitialised, a block's pages that no record has
/// reached yet are never touched, so never charged. Per-shard arenas had small heap blocks whose
/// unused tails shared pages with other allocations (3.3 MB allocated for 1.9 MB of records at full
/// scale). Measured (sdrig, this VM, alternating A/B against per-shard arenas): M 15.8 -> 14.5 MiB, T
/// unchanged (0.377-0.413 vs 0.404-0.413 s). Each shard reserves RESERVE bytes at a
/// time under the arena's lock and writes its records there under its own lock, so the arena lock is
/// taken about once per 50 inserts: with it taken on every insert, T rose 10-17 % on this VM (12
/// workers inserting most names in the first round). It is always taken after a shard's lock.
#[derive(Default)]
struct Arena {
    blocks: [AtomicPtr<u8>; 32],
    writer: ArenaWriter,
}

/// The arena's insert side, on its own cache line so inserts do not disturb readers of `blocks`.
#[repr(align(64))]
#[derive(Default)]
struct ArenaWriter(Mutex<ArenaBlocks>);

/// The blocks the directory points into, owned here so they are freed with the Intern, and the next
/// free offset. Block bytes are `UnsafeCell`s because readers hold pointers into a block while
/// the writer appends behind them: the writer only writes bytes no published record covers, through
/// raw pointers. They start uninitialised: nothing reads a byte before a record is written over it.
#[derive(Default)]
struct ArenaBlocks {
    blocks: Vec<Box<[MaybeUninit<UnsafeCell<u8>>]>>,
    used: usize,
}

/// The first arena block's size; block k is this times 2^k.
const ARENA_BLOCK: usize = 4 << 10;

/// Offset where block k starts: the sizes of blocks 0..k summed.
fn block_start(k: usize) -> usize {
    ARENA_BLOCK * ((1 << k) - 1)
}

/// The block an offset falls in.
fn block_of(offset: usize) -> usize {
    (offset / ARENA_BLOCK + 1).ilog2() as usize
}

impl Arena {
    /// Reserves `len` bytes (a multiple of the 1 KiB reserve step, so a reservation never straddles
    /// a block; blocks are multiples of it too) and returns their offset. Allocates on first use.
    fn reserve(&self, len: usize) -> usize {
        let mut a = self.writer.0.lock().unwrap();
        // Skip to the first block whose remainder holds the reservation (blocks double, so this ends).
        while a.used + len > block_start(block_of(a.used) + 1) {
            a.used = block_start(block_of(a.used) + 1);
        }
        let k = block_of(a.used);
        // A block is allocated when a reservation first lands in it, decided by block k's own
        // directory pointer. The skip above can pass over several blocks (a reservation longer than
        // the next block, e.g. a name of 4 KiB or more, c-join.md:50), so testing `blocks.len() <= k`
        // instead allocated block k again on its next reservation and overwrote its pointer, losing
        // the records already in it (names_distinct wrong from 2 workers up; kit group intern-edge,
        // check/c-kit bf91502). `blocks` only owns the allocations.
        if self.blocks[k].load(Ordering::Relaxed).is_null() {
            let block = Box::<[UnsafeCell<u8>]>::new_uninit_slice(ARENA_BLOCK << k);
            self.blocks[k].store(block.as_ptr() as *mut u8, Ordering::Relaxed);
            a.blocks.push(block);
        }
        a.used += len;
        a.used - len
    }
}

/// What only the inserting thread touches, under the shard's lock.
#[derive(Default)]
struct Writer {
    /// The current generation of slots, alone: a grown-out one moves to `Intern::retired`, because a
    /// lock-free reader may still be probing it.
    slots: Box<[AtomicU64]>,
    names: usize,
    /// The shard's current arena reservation: records go at `next`, up to `end`.
    next: usize,
    end: usize,
}

impl Writer {
    /// Writes the record for `key` with `id` into this shard's reservation, reserving more first if
    /// it does not fit; returns its offset.
    fn store(&mut self, arena: &Arena, key: &[u8], id: u32) -> *mut u8 {
        // At least 8 name bytes, zero beyond the name, so `matches` can read a whole word.
        let need = 8 + key.len().max(8);
        if self.next + need > self.end {
            let len = need.next_multiple_of(1 << 10); // the reserve step: 1 KiB
            self.next = arena.reserve(len);
            self.end = self.next + len;
        }
        // Relaxed: the block was allocated under the arena lock before this offset was reserved.
        let p = arena.blocks[block_of(self.next)].load(Ordering::Relaxed);
        let at = unsafe { p.add(self.next - block_start(block_of(self.next))) };
        // The record's 8-byte head `[len: u32 LE][id: u32 LE]`, written below as one word.
        let head = ((id as u64) << 32 | key.len() as u64).to_le_bytes();
        // SAFETY: the record lies inside this shard's reservation, which only this shard writes,
        // under its lock, and no published slot names these bytes yet, so nothing reads them.
        unsafe {
            ptr::copy_nonoverlapping(head.as_ptr(), at, 8);
            ptr::copy_nonoverlapping(key.as_ptr(), at.add(8), key.len());
            ptr::write_bytes(at.add(8 + key.len()), 0, need - 8 - key.len());
        }
        self.next += need;
        at
    }

    /// Doubles the slots: copies every published slot into a new generation (only this thread
    /// writes slots, so plain loads suffice) and publishes it; readers still on the old generation
    /// find every name it held, and a miss there sends them to the locked re-check. Returns the old one.
    fn grow(&mut self, current: &AtomicU64) -> Box<[AtomicU64]> {
        let new = new_slots(self.slots.len() * 2);
        for entry in self.slots.iter().filter(|s| s.load(Ordering::Relaxed) != 0) {
            let slot = entry.load(Ordering::Relaxed);
            // SAFETY: a published slot names a complete record (`Writer::store`).
            let p = record(slot);
            let len = u32::from_le_bytes(unsafe { *(p as *const [u8; 4]) }) as usize;
            let name = unsafe { std::slice::from_raw_parts(p.add(8), len) };
            let mut pos = hash(name, 0, name.len()).0 as usize & (new.len() - 1);
            while new[pos].load(Ordering::Relaxed) != 0 {
                pos = (pos + 1) & (new.len() - 1);
            }
            new[pos].store(slot, Ordering::Relaxed);
        }
        // Release: a reader that loads this pointer (Acquire) sees every slot copied above.
        current.store(packed(&new), Ordering::Release);
        std::mem::replace(&mut self.slots, new)
    }
}

/// Shards are picked by the hash's top bits, so 12 workers rarely wait on the same lock.
const SHARD_BITS: u32 = 8;

/// One shard on its own cache line. Hits read `current` and the slots without locking; the mutex
/// is taken only to insert. Hits are about 97 % of lookups (100.5k distinct names in 3.32M records,
/// seed 1), so the lock and its cache-line traffic leave the common path.
#[repr(align(64))]
struct Shard {
    /// The current generation, as `packed`.
    current: AtomicU64,
    writer: Mutex<Writer>,
}

/// Raw name bytes (between the quotes, escapes kept) -> dense id. `""` -> `"` is one to one, so raw
/// bytes are distinct exactly when the unescaped values are; `""` and an empty unquoted field are
/// both the empty key. Every hit is an exact byte compare against the copy in the arena.
#[derive(Default)]
pub struct Intern {
    shards: Vec<Shard>,
    arena: Arena,
    next_id: AtomicU32,
    /// Retired slot generations, each with the era it was retired in, freed by `quiesce`.
    retired: Mutex<Vec<(u64, Box<[AtomicU64]>)>>,
    era: AtomicU64,
}

impl Intern {
    /// `slots_per_shard` is each shard's starting size (a power of two, grown by doubling at half load).
    pub fn new(slots_per_shard: usize) -> Intern {
        let len = slots_per_shard.next_power_of_two().max(2);
        let shards = (0..1 << SHARD_BITS)
            .map(|_| {
                let slots = new_slots(len);
                Shard { current: AtomicU64::new(packed(&slots)), writer: Mutex::new(Writer { slots, ..Default::default() }) }
            })
            .collect();
        Intern { shards, ..Default::default() }
    }

    /// Worker `me` is between chunks, holding no slot generation (a lookup holds one only until it
    /// returns): it records the era it has reached (`u64::MAX` once `done`), then frees every retired
    /// generation older than the era all workers have reached. Safe because growth publishes the new
    /// generation (Release) before taking a retirement era (SeqCst), so a worker that has seen that
    /// era at a boundary starts every later lookup from the new generation and finished every earlier
    /// one. Freed per chunk instead of at the end, as pass 2 has no barrier: sdrig M 15.8 -> 15.2 MiB
    /// on this VM (c-19a2 -> c-19a3), box in the commit log.
    pub fn quiesce(&self, seen: &[AtomicU64], me: usize, done: bool) {
        let reached = if done { u64::MAX } else { self.era.load(Ordering::SeqCst) };
        seen[me].store(reached, Ordering::SeqCst);
        let safe = seen.iter().fold(u64::MAX, |lowest, s| lowest.min(s.load(Ordering::SeqCst)));
        self.retired.lock().unwrap().retain(|&(era, _)| era >= safe);
    }

    /// Hashes the name `b[start..start + len]` for `id` and prefetches its home slot, while the parse
    /// goes on with the rest of the record (note, region, amount), so the slot's cache miss overlaps
    /// that work instead of stalling the lookup. One prefetch per record, issued from the parse: unlike
    /// c-8's ring, no queue and no extra state. Only a hint; it never faults. Measured on box 2: T 0.061 ->
    /// 0.057 s, K 22.09 -> 22.76 (c-22).
    pub fn prefetch(&self, b: &[u8], start: usize, len: usize) -> (u64, u64, u64) {
        let hashed = hash(b, start, len);
        let shard = &self.shards[(hashed.0 >> (64 - SHARD_BITS)) as usize];
        let c = shard.current.load(Ordering::Acquire);
        let slot = (c as usize & ((1 << 56) - 1)) + (hashed.0 as usize & ((1 << (c >> 56)) - 1)) * 8;
        // SAFETY: a prefetch is a hint; it reads nothing architecturally and never faults.
        unsafe {
            std::arch::x86_64::_mm_prefetch(slot as *const i8, std::arch::x86_64::_MM_HINT_T0)
        };
        hashed
    }

        /// The id of `b[start..start + len]`, inserting it if it is new.
    pub fn id(&self, b: &[u8], start: usize, len: usize, (h, lo, hi): (u64, u64, u64)) -> u32 {
        let key = (&b[start..start + len], lo, hi);
        let shard = &self.shards[(h >> (64 - SHARD_BITS)) as usize];
        let c = shard.current.load(Ordering::Acquire);
        let base = std::ptr::with_exposed_provenance::<AtomicU64>(c as usize & ((1 << 56) - 1));
        // SAFETY: `current` always holds a generation owned by the shard's writer or retired to
        // `Intern::retired`, and one is freed only by `quiesce`, after every worker has passed a chunk
        // boundary since its retirement, so not while this lookup can hold it. Reaching the slots from
        // this one word instead of through the array's header measured on box 2: T 0.064 -> 0.061-0.062 s (c-20a).
        let slots = unsafe { std::slice::from_raw_parts(base, 1 << (c >> 56)) };
        if let Ok(id) = find(slots, key, h) {
            return id;
        }
        // Miss: take the lock, re-check the current generation (another worker may have inserted
        // it, or grown the table), then insert and publish.
        let mut w = shard.writer.lock().unwrap();
        let mut pos = match find(&w.slots, key, h) {
            Ok(id) => return id,
            Err(pos) => pos,
        };
        // Grow before this insert would fill the generation past half. Every generation a reader can
        // reach then keeps an empty slot, which is what ends a lock-free probe for a name it lacks: a
        // full retired generation (2 slots, both used) made such a probe spin forever, found by the
        // stress runs at SD_INTERN_SLOTS=2.
        if (w.names + 1) * 2 > w.slots.len() {
            let old = w.grow(&shard.current);
            let era = self.era.fetch_add(1, Ordering::SeqCst);
            self.retired.lock().unwrap().push((era, old));
            pos = find(&w.slots, key, h).expect_err("name absent before the grow");
        }
        // New name: its id is taken under the shard lock, so ids stay dense across shards.
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let addr = w.store(&self.arena, key.0, id).expose_provenance() as u64;
        // Release: a reader that sees this slot (Acquire) sees the record and its block pointer.
        w.slots[pos].store((h >> 32 & 0xFFFF) << 48 | addr, Ordering::Release);
        w.names += 1;
        id
    }
}

/// A set of name ids, grown on demand.
#[derive(Default, Clone)]
pub struct NameSet(Vec<u64>);

impl NameSet {
    pub fn insert(&mut self, id: u32) {
        let word = (id / 64) as usize;
        if word >= self.0.len() {
            // Growth step 128 words (1 KiB): few reallocations, and at most 1 KiB of slack per group.
            let len = (word + 1).next_multiple_of(128);
            self.0.reserve_exact(len - self.0.len());
            self.0.resize(len, 0);
        }
        self.0[word] |= 1 << (id % 64);
    }

    pub fn len(&self) -> u64 {
        self.0.iter().map(|w| w.count_ones() as u64).sum()
    }
}
