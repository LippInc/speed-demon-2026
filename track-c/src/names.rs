//! Distinct names: one intern table shared by every worker gives each distinct name a dense id
//! (0 upward), and each group keeps a bitset over those ids (pass2.rs), so names_distinct is the
//! bitset's popcount.

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
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

/// One slot: 0 while empty, else `(low 32 hash bits) << 32 | (record offset + 1)`, where the record is
/// the name's entry in the arena (`Arena`). 8 bytes, where c-6's slot was 16 (meta plus a name
/// pointer), and records replace a per-name entry: measured (sdrig, this VM) M 19.5 -> 15.6 MiB, T
/// unchanged (0.386 -> 0.381 s). The stored hash bits pick the home slot again when the table
/// doubles, and filter probes before the byte compare.
type Slot = AtomicU64;

/// One generation of a shard's slots (a power of two of them).
struct Slots {
    mask: usize,
    slots: Box<[Slot]>,
}

impl Slots {
    fn new(len: usize) -> Box<Slots> {
        Box::new(Slots { mask: len - 1, slots: (0..len).map(|_| AtomicU64::new(0)).collect() })
    }

    /// Probes for `key` from its home slot. Returns its id, or the index of the empty slot that
    /// ends the probe. A slot is published (record written, then the slot stored with Release) before
    /// it is seen non-zero here (Acquire), so the record it names is complete.
    #[inline]
    fn find(&self, arena: &Arena, key: &[u8], tag: u32) -> Result<u32, usize> {
        let mut pos = tag as usize & self.mask;
        loop {
            let slot = self.slots[pos].load(Ordering::Acquire);
            if slot == 0 {
                return Err(pos);
            }
            if (slot >> 32) as u32 == tag {
                // SAFETY: a published slot names a complete record in an arena block that lives,
                // unmoved, as long as the Intern does.
                let (stored, id) = unsafe { arena.record(slot as u32 - 1) };
                if stored == key {
                    return Ok(id);
                }
            }
            pos = (pos + 1) & self.mask;
        }
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
struct Arena {
    blocks: [AtomicPtr<u8>; 32],
    writer: ArenaWriter,
}

/// The arena's insert side, on its own cache line so inserts do not disturb readers of `blocks`.
#[repr(align(64))]
struct ArenaWriter(Mutex<ArenaBlocks>);

/// The blocks the directory points into, owned here so they are freed with the Intern, and the next
/// free offset. Block bytes are `UnsafeCell`s because readers hold pointers into a block while
/// the writer appends behind them: the writer only writes bytes no published record covers, through
/// raw pointers. They start uninitialised: nothing reads a byte before a record is written over it.
struct ArenaBlocks {
    blocks: Vec<Box<[MaybeUninit<UnsafeCell<u8>>]>>,
    used: usize,
}

/// The first arena block's size; block k is this times 2^k.
const ARENA_BLOCK: usize = 4 << 10;

/// What a shard reserves from the arena at a time (a divisor of ARENA_BLOCK). A shard's unused
/// reservation at the end is at most this, so 256 KiB over 256 shards.
const RESERVE: usize = 1 << 10;

/// Offset where block k starts: the sizes of blocks 0..k summed.
fn block_start(k: usize) -> usize {
    ARENA_BLOCK * ((1 << k) - 1)
}

/// The block an offset falls in.
fn block_of(offset: usize) -> usize {
    (offset / ARENA_BLOCK + 1).ilog2() as usize
}

impl Arena {
    fn new() -> Arena {
        Arena {
            blocks: std::array::from_fn(|_| AtomicPtr::new(ptr::null_mut())),
            writer: ArenaWriter(Mutex::new(ArenaBlocks { blocks: Vec::new(), used: 0 })),
        }
    }

    /// The name bytes and id of the record at `offset`.
    ///
    /// # Safety
    /// `offset` must come from a slot published after its record was written (`Writer::store`).
    #[inline]
    unsafe fn record(&self, offset: u32) -> (&[u8], u32) {
        let offset = offset as usize;
        let k = block_of(offset);
        // Relaxed: the block pointer was stored before the slot that names this record was
        // published with Release, and the caller loaded that slot with Acquire.
        let p = unsafe { self.blocks[k].load(Ordering::Relaxed).add(offset - block_start(k)) };
        unsafe {
            let len = u32::from_le_bytes(*(p as *const [u8; 4])) as usize;
            let id = u32::from_le_bytes(*(p.add(4) as *const [u8; 4]));
            (std::slice::from_raw_parts(p.add(8), len), id)
        }
    }

    /// Reserves `len` bytes (a multiple of RESERVE, so a reservation never straddles a block; blocks
    /// are multiples of it too) and returns their offset. Allocates the block on first use.
    fn reserve(&self, len: usize) -> usize {
        let mut a = self.writer.0.lock().unwrap();
        // Skip to the first block whose remainder holds the reservation (blocks double, so this ends).
        while a.used + len > block_start(block_of(a.used) + 1) {
            a.used = block_start(block_of(a.used) + 1);
        }
        assert!(a.used + len < u32::MAX as usize, "names over 4 GiB");
        let k = block_of(a.used);
        if a.blocks.len() <= k {
            // A block is allocated only when a reservation first lands in it (k = blocks.len() here,
            // since `used` only moves forward one block at a time).
            let block = Box::<[UnsafeCell<u8>]>::new_uninit_slice(ARENA_BLOCK << k);
            self.blocks[k].store(block.as_ptr() as *mut u8, Ordering::Relaxed);
            a.blocks.push(block);
        }
        let offset = a.used;
        a.used += len;
        offset
    }

    /// Where offset `offset` is, for the shard that reserved it to write a record there.
    fn at(&self, offset: usize) -> *mut u8 {
        let k = block_of(offset);
        // Relaxed: the block was allocated under the arena lock before this offset was reserved.
        unsafe { self.blocks[k].load(Ordering::Relaxed).add(offset - block_start(k)) }
    }
}

/// What only the inserting thread touches, under the shard's lock.
struct Writer {
    /// Every generation of slots, the current one last. Older ones are kept until the next quiescent
    /// point (`free_retired`), because a lock-free reader may still be probing one.
    generations: Vec<Box<Slots>>,
    names: usize,
    /// The shard's current arena reservation: records go at `next`, up to `end`.
    next: usize,
    end: usize,
}

impl Writer {
    /// Writes the record for `key` with `id` into this shard's reservation, reserving more first if
    /// it does not fit; returns its offset.
    fn store(&mut self, arena: &Arena, key: &[u8], id: u32) -> u32 {
        let need = 8 + key.len();
        if self.next + need > self.end {
            let len = need.next_multiple_of(RESERVE);
            self.next = arena.reserve(len);
            self.end = self.next + len;
        }
        // SAFETY: the record lies inside this shard's reservation, which only this shard writes,
        // under its lock, and no published slot names these bytes yet, so nothing reads them.
        unsafe {
            let at = arena.at(self.next);
            ptr::copy_nonoverlapping((key.len() as u32).to_le_bytes().as_ptr(), at, 4);
            ptr::copy_nonoverlapping(id.to_le_bytes().as_ptr(), at.add(4), 4);
            ptr::copy_nonoverlapping(key.as_ptr(), at.add(8), key.len());
        }
        let offset = self.next as u32;
        self.next += need;
        offset
    }

    /// Doubles the slots: copies every published slot into a new generation (only this thread
    /// writes slots, so plain loads suffice) and publishes it; readers still on the old generation
    /// find every name it held, and a miss there sends them to the locked re-check.
    fn grow(&mut self, current: &AtomicPtr<Slots>) {
        let old = self.generations.last().unwrap();
        let new = Slots::new(old.slots.len() * 2);
        for slot in old.slots.iter() {
            let slot = slot.load(Ordering::Relaxed);
            if slot != 0 {
                let mut pos = (slot >> 32) as usize & new.mask;
                while new.slots[pos].load(Ordering::Relaxed) != 0 {
                    pos = (pos + 1) & new.mask;
                }
                new.slots[pos].store(slot, Ordering::Relaxed);
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
/// both the empty key. Every hit is an exact byte compare against the copy in the arena.
pub struct Intern {
    shards: Vec<Shard>,
    arena: Arena,
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
                    writer: Mutex::new(Writer { generations: vec![first], names: 0, next: 0, end: 0 }),
                }
            })
            .collect();
        Intern { shards, arena: Arena::new(), next_id: AtomicU32::new(0) }
    }

    /// Frees every shard's retired slot generations, keeping only the current one.
    ///
    /// # Safety
    /// No `id` call may be in flight on any thread, and none may start until this returns: a lookup
    /// can hold a pointer to any generation it loaded from `current`, but only until it returns.
    pub unsafe fn free_retired(&self) {
        for shard in &self.shards {
            let mut w = shard.writer.lock().unwrap();
            let current = w.generations.pop().unwrap();
            w.generations.clear();
            w.generations.push(current);
        }
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
        // SAFETY: `current` always points at a generation owned by the shard's writer; a generation
        // this lookup can reach is freed only at a quiescent point, when no lookup is in flight.
        let slots = unsafe { &*shard.current.load(Ordering::Acquire) };
        if let Ok(id) = slots.find(&self.arena, key, tag) {
            return id;
        }
        // Miss: take the lock, re-check the current generation (another worker may have inserted
        // it, or grown the table), then insert and publish.
        let mut w = shard.writer.lock().unwrap();
        let mut pos = match w.generations.last().unwrap().find(&self.arena, key, tag) {
            Ok(id) => return id,
            Err(pos) => pos,
        };
        // Grow before this insert would fill the generation past half. Every generation a reader can
        // reach then keeps an empty slot, which is what ends a lock-free probe for a name it lacks: a
        // full retired generation (2 slots, both used) made such a probe spin forever, found by the
        // stress runs at SD_INTERN_SLOTS=2.
        if (w.names + 1) * 2 > w.generations.last().unwrap().slots.len() {
            w.grow(&shard.current);
            pos = w.generations.last().unwrap().find(&self.arena, key, tag).expect_err("name absent before the grow");
        }
        // New name: its id is taken under the shard lock, so ids stay dense across shards.
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        assert!(id < u32::MAX, "more than 2^32 - 1 distinct names");
        let offset = w.store(&self.arena, key, id);
        // Release: a reader that sees this slot (Acquire) sees the record and its block pointer.
        w.generations.last().unwrap().slots[pos].store((tag as u64) << 32 | (offset as u64 + 1), Ordering::Release);
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
