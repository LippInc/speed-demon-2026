//! Track A: per (hour, endpoint) request count, nearest-rank p50/p95/p99 latency and 5xx rate.
//! The threads claim an mmap of /data/requests.log in 1 MiB blocks, in file order, and write each line as a 32-bit
//! row into its hour's region. An hour is finished (counted, sorted, formatted) as soon as the registered prefix of
//! the file holds a line two hours later, while other threads keep parsing; its text is written over its rows, placed
//! in hour order and written at its offset, and the region pooled for a later hour: only a few hours' rows are alive at
//! once. Main tricks: parse and line (a-62), the endpoint table (a-63), finish and deal (a-61), rows and endpoints in
//! Job (a-59, a-60), the pipeline (a-55).
#![allow(unsafe_op_in_unsafe_fn)]
use std::arch::x86_64::*;
use std::mem::{replace, take};
use std::ptr::copy_nonoverlapping as copy;
use std::slice::{from_raw_parts, from_raw_parts_mut};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::*};
use std::sync::{Condvar, Mutex};
use std::{collections::BTreeMap, fs::File, os::fd::AsRawFd, os::unix::fs::FileExt};

// Spec 2: every timestamp lies in the same 72 hours for every seed and scale, 2026-10-30T18:00Z to 2026-11-02T17:59Z.
const HOURS: usize = 72;
const DAYS: [&str; 4] = ["2026-10-30", "2026-10-31", "2026-11-01", "2026-11-02"];
// Spec 2: exactly 1,842 distinct endpoints per seed, so global ids stay below 2,048 (11 bits).
const IDS: usize = 2048;
// 1 MiB: two blocks share a 2 MiB page table, so at most two threads fault in its pages under its lock at once (256 KiB
// blocks: eight; that lock took 16-18 pct of the kernel's cycles, box 16.29 -> 15.42 ms, a-64).
const BLOCK: usize = 1 << 20;
// An hour is finished once the registered prefix of the file holds a line LAG hours later. Spec 2 promises no bound on
// how far a line is out of order (generated files: at most one hour behind the latest hour before it), so a line that
// comes after its hour was finished is detected, and main repeats the run with no early finishing: exact for any order.
const LAG: usize = 2;

unsafe extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn madvise(addr: *const u8, len: usize, advice: i32) -> i32;
    fn prctl(option: i32, ...) -> i32;
}

/// Byte mask of `byte` in the 32 bytes at p, one bit per byte. Safety: p..p + 32 must be readable.
unsafe fn mask32(p: *const u8, byte: u8) -> u32 {
    // AVX2 is enabled at compile time (x86-64-v3).
    let v = _mm256_loadu_si256(p as _);
    _mm256_movemask_epi8(_mm256_cmpeq_epi8(v, _mm256_set1_epi8(byte as i8))) as u32
}

/// The value of 8 decimal digits, one per byte (0 to 9), the most significant in the low byte: two multiply-add steps
/// fold digit pairs, then 2-digit pairs, into two 4-digit halves.
unsafe fn digits8(v: u64) -> u64 {
    // SSE2 and SSSE3 are part of the x86-64-v3 baseline.
    let pairs = _mm_maddubs_epi16(_mm_cvtsi64_si128(v as i64), _mm_set1_epi16(0x010a));
    let r = _mm_cvtsi128_si64(_mm_madd_epi16(pairs, _mm_set1_epi32(0x0001_0064))) as u64;
    (r & 0xffff_ffff) * 10_000 + (r >> 32)
}

/// The endpoint's start and length, the 5xx flag and the latency of the line at p, and the next line's start (spec 2:
/// "<timestamp> <method> <endpoint> <status> <latency>", the timestamp always 27 bytes). A 64-byte space mask from
/// the method on holds the space after the method (at most 7 bytes) and, unless the endpoint is long, the one after it;
/// the 3-digit status follows, its first digit '5' exactly for a 5xx, then the latency, whose digits one 8-byte load
/// finds up to its LF. Read forward, the next line's start comes out of the latency's length: no LF search first.
/// Safety: d + p..d + p + 128 must be readable, and so must the line's end + 32.
#[inline(always)]
unsafe fn line(d: *const u8, p: usize) -> (usize, usize, u64, u32, usize) {
    let mut at = p + 28;
    let m = mask32(d.add(at), b' ') as u64 | (mask32(d.add(at + 32), b' ') as u64) << 32;
    let ep = at + 1 + m.trailing_zeros() as usize;
    // The space after the endpoint; past the first 64 bytes, 32 at a time (bits stay relative to `at`).
    let mut rest = m & m.wrapping_sub(1);
    while rest == 0 {
        at += 32;
        rest = (mask32(d.add(at + 32), b' ') as u64) << 32;
    }
    let sp = at + rest.trailing_zeros() as usize;
    let v = (d.add(sp + 5) as *const u64).read_unaligned() ^ 0x3030_3030_3030_3030;
    // After the xor a byte is a digit exactly when it is below 10; adding 0x76 sets the top bit of every other one.
    let other = (v.wrapping_add(0x7676_7676_7676_7676) | v) & 0x8080_8080_8080_8080;
    let n = other.trailing_zeros() as usize / 8;
    let (latency, next) = if other == 0 {
        // 8 digits or more (spec 2: at most 10): the rest one by one.
        let (mut latency, mut at) = (digits8(v), sp + 13);
        while *d.add(at) != b'\n' {
            latency = latency * 10 + (*d.add(at) - b'0') as u64;
            at += 1;
        }
        (latency, at + 1)
    } else {
        (digits8(v << (64 - 8 * n)), sp + 6 + n)
    };
    let error = (*d.add(sp + 1) == b'5') as u64;
    (ep, sp - ep, error, latency as u32, next)
}

/// A 64-bit hash of the whole len-byte endpoint at p: its first and last 32 bytes, each cut to the endpoint, and from 65
/// bytes on the two loads over its middle (spec 2: at most 125 bytes, so four loads cover it), mixed by AES rounds that
/// work on both 16-byte halves at once, the length a round key. Two rounds follow the last input, which spreads every
/// input bit over the whole half. Safety: p - 32..p + 128 must be readable.
#[inline(always)]
unsafe fn ep_hash(p: *const u8, len: usize) -> u64 {
    let load = |at: *const u8| _mm256_loadu_si256(at as _);
    // The lane numbers 0 to 31 (a constant once compiled) and the length, to cut a load to the endpoint's bytes.
    let iota = load(std::array::from_fn::<u8, 32, _>(|i| i as u8).as_ptr());
    let n = _mm256_set1_epi8(len.min(32) as i8);
    // Byte i of the first load is the endpoint's when i < n, of the last load when i + n > 31.
    let first = _mm256_and_si256(load(p), _mm256_cmpgt_epi8(n, iota));
    let last_in = _mm256_cmpgt_epi8(_mm256_add_epi8(iota, n), _mm256_set1_epi8(31));
    let mut x = _mm256_aesenc_epi128(first, _mm256_and_si256(load(p.add(len).sub(32)), last_in));
    if len > 64 {
        x = _mm256_aesenc_epi128(x, load(p.add(32)));
        x = _mm256_aesenc_epi128(x, load(p.add(len - 64)));
    }
    x = _mm256_aesenc_epi128(x, _mm256_set1_epi64x(len as i64));
    x = _mm256_aesenc_epi128(x, _mm256_set1_epi64x(0x5851_f42d_4c95_7f2d));
    let x = _mm_xor_si128(_mm256_castsi256_si128(x), _mm256_extracti128_si256::<1>(x));
    (_mm_cvtsi128_si64(x) ^ _mm_extract_epi64::<1>(x)) as u64
}

/// An endpoint's bytes, zero-padded to 128 (spec 2: 1 to 125 bytes, never a zero byte), its length in byte 127: copied
/// out in whole 32-byte chunks, and padded like this two endpoints order as their bytes do.
type Name = [u8; 128];

/// A thread's open chunk of an hour's rows: the next row and the chunk's end.
type Chunk = (*mut u32, *mut u32);

/// Slots of the endpoint table: 8,192 stay under a quarter full (spec 2: 1,842 endpoints), so a probe nearly always
/// ends at the first slot.
const SLOTS: usize = 8192;

/// A row is (endpoint id + 1) << 21 | 5xx << 20 | latency, 0 for a slot no line took; a line with a latency of ESCAPE
/// or more (0.24 pct on seed 1) goes to its hour's long list instead, as latency << 32 | the row without the latency.
const ESCAPE: u32 = (1 << 20) - 1;
/// Rows a thread claims in one hour's region at a time.
const CLAIM: usize = 256;
/// An hour's text is written over its rows, so the latencies the finish copies out start at least this many rows
/// (512 KiB) in: past the longest text (spec 2: 1,842 endpoints of at most 213 bytes a line).
const SPARE: usize = 1 << 17;

/// The run's shared progress, under one lock, with the finished hours waiting for the ones before them, how many are
/// placed, and where the next one goes.
#[derive(Default)]
struct State {
    next_block: usize,
    top: BTreeMap<usize, usize>, // per registered block past the watermark: its last hour
    watermark: usize,            // every block below it is registered
    prefix_max: usize,           // the last hour in the blocks below the watermark
    next_hour: usize,            // every hour below it is claimed for finishing
    late: bool,                  // a block brought lines of an hour already claimed
    parts: BTreeMap<usize, &'static [u8]>,
    placed: usize,
    offset: u64,
}

/// One run over the input (a-55). Each thread takes, in this order: an hour that is safe to finish, else the next
/// block, at most two per thread past the oldest unregistered one, so few records wait; else it sleeps until a block
/// is registered or an hour claimed; once all hours are claimed it releases the mapping and ends. Unlike a-37 (static
/// ranges, finishing on the scan threads, then a barrier and a serial merge), no thread idles while work is left, and
/// the finishing fills the gaps. Measured effect: PERF.md a-55.
///
/// Every hour's rows: a region each, from an anonymous reservation (MAP_NORESERVE) of which only the pages touched
/// cost memory. Threads claim CLAIM rows at a time, so a row needs no atomic; an hour's region holds its rows, then
/// its latencies grouped by endpoint. Once its text is written the region goes to the pool, which keeps one, and a
/// later hour takes it, pages already faulted in: a fresh region per hour cost 7.8K page faults (a-59). In the steady
/// state one hour's region goes in as the next hour's comes out, so one keeps nearly every page fault away (box, page
/// faults: none 13.9K, one 6.98K, unbounded 6.50K) for ~+0.6 MB of peak; an unbounded pool grew to 11 regions, +3.8 MB.
///
/// Every endpoint met so far, one table for all threads, open addressing with linear probing. A slot is the hash's top
/// 53 bits and the id + 1 in the low 11, 0 when empty: the hash alone is an endpoint's identity, and for 1,842 of them
/// the chance that two share those bits is about 2e-10. Slots are only filled, under the lock of `names`, so a slot
/// seen empty there ends the probe; the table fills in the run's first moments and is then only read, one copy in
/// every core's cache. A slot carries its id, so its load needs no ordering. The names, by id, are pushed under the lock
/// into room for IDS made up front, so the Vec never moves and a name, once pushed, is never written again: readers
/// keep a pointer to them.
struct Job<'a> {
    data: &'static [u8],
    file: &'a File,
    state: Mutex<State>,
    ready: Condvar,
    released: AtomicUsize,
    base: *mut u32,
    stride: usize,
    regions: [AtomicUsize; HOURS], // 0 until the hour takes one
    pool: Mutex<(usize, usize)>, // the pooled region (0: none); regions taken from the reservation
    claimed: [AtomicUsize; HOURS],
    long: [Mutex<Vec<u64>>; HOURS],
    slots: [AtomicU64; SLOTS],
    names: Mutex<(Vec<Name>, Vec<u16>)>, // by id; the ids in byte order, as last asked for
}

// Safety: threads write only the row chunks they claimed; an hour's region is read only by the thread finishing it;
// slots are only filled under the lock of `names`, so a slot seen empty there ends the probe; a name is written once,
// under that lock, before its slot is.
unsafe impl Sync for Job<'_> {}

impl Job<'_> {
    /// The id of the len-byte endpoint at p, a new one the first time it is met; p - 32..p + 128 must be readable.
    #[inline(always)]
    fn id(&self, p: *const u8, len: usize) -> u32 {
        // Bit 11 set: no hash matches an empty slot. Safety: as the caller promises.
        let h = unsafe { ep_hash(p, len) } | 1 << 11;
        let mut i = (h >> 51) as usize;
        loop {
            match self.slots[i].load(Relaxed) {
                s if s >> 11 == h >> 11 => return (s & 2047) as u32 - 1,
                0 => self.add(p, len, h, i),
                _ => i = (i + 1) % SLOTS,
            }
        }
    }

    /// A new id in slot i, under the lock, unless another thread filled it first (the probe then reads it again).
    #[cold]
    fn add(&self, p: *const u8, len: usize, h: u64, i: usize) {
        let mut names = self.names.lock().unwrap();
        if self.slots[i].load(Relaxed) == 0 {
            assert!(names.0.len() < IDS - 1, "more endpoints than spec 2 allows");
            let mut name = [0; 128];
            // Safety: the endpoint is the len bytes at p.
            name[..len].copy_from_slice(unsafe { from_raw_parts(p, len) });
            name[127] = len as u8;
            names.0.push(name);
            self.slots[i].store(h >> 11 << 11 | names.0.len() as u64, Relaxed);
        }
    }

    /// The id of every endpoint met, in byte order (spec 4), and the names by id: the ids new since the last call are
    /// appended and sorted in, a stable sort that finds the sorted prefix as one run.
    fn order(&self) -> (Vec<u16>, *const Name) {
        let mut guard = self.names.lock().unwrap();
        let (names, order) = &mut *guard;
        if order.len() < names.len() {
            order.extend(order.len() as u16..names.len() as u16);
            order.sort_by_key(|&id| &names[id as usize]);
        }
        (order.clone(), names.as_ptr())
    }

    /// Hour h's region; the first thread to ask gives it one. Under the pool's lock, which every write of `regions`
    /// holds: the pooled region, else the reservation's next one (a run touches ~11 of them; an hour's own region at
    /// h * stride touched all 72, +0.4 MB of page tables).
    fn region(&self, h: usize) -> *mut u32 {
        if self.regions[h].load(Acquire) == 0 {
            let mut pool = self.pool.lock().unwrap();
            if self.regions[h].load(Relaxed) == 0 {
                let fresh = self.base.wrapping_add(pool.1 * self.stride) as usize;
                let r = Some(take(&mut pool.0)).filter(|&r| r > 0).unwrap_or(fresh);
                pool.1 += (r == fresh) as usize;
                self.regions[h].store(r, Release);
            }
        }
        self.regions[h].load(Relaxed) as *mut u32
    }

    /// Block b's first line start: the first at or after b * BLOCK, or the input's end when no LF is left (spec 2: the
    /// last line ends with an LF); 0 for block 0.
    fn align(&self, b: usize) -> usize {
        let n = (b * BLOCK).min(self.data.len());
        let tail = &self.data[n - (n > 0) as usize..];
        let lf = tail.iter().position(|&c| c == b'\n');
        lf.map_or(self.data.len(), |o| n + o) * (n > 0) as usize
    }

    /// Writes a row for every line starting in data[start..end] into its hour's region, through cur, this thread's open
    /// chunk per hour, and returns the lowest and highest hour met. Reads may pass the input's end by up to 128 bytes
    /// (main maps zeros there) and start up to 32 bytes before an endpoint, inside its line.
    fn parse(&self, (start, end): (usize, usize), cur: &mut [Chunk; HOURS]) -> (usize, usize) {
        let d = self.data.as_ptr();
        let (mut prev_prefix, mut p, mut h, mut low, mut high) = (u64::MAX, start, 0, HOURS, 0);
        let mut c = cur[0]; // hour h's chunk: next row, last row but one
        while p < end {
            // Lines usually share the previous line's hour, so compare it first: spec 2's year is always 2026, so bytes
            // 5..13 ("MM-DDTHH") name the hour, one u64 load (a-56). Two lines ahead are asked for meanwhile: the
            // prefetcher stops at each 4 KiB page (box: 18.12 -> 17.69 ms).
            // Safety: a line has at least 40 bytes; a prefetch never faults.
            let prefix = unsafe {
                _mm_prefetch::<_MM_HINT_T0>(d.add(p + 1280) as _);
                _mm_prefetch::<_MM_HINT_T0>(d.add(p + 1344) as _);
                (d.add(p + 5) as *const u64).read_unaligned()
            };
            if prefix != prev_prefix {
                cur[h] = c;
                // Hours since 2026-10-30T18, from "YYYY-MM-DDTHH" (spec 2: the window crosses two midnights and the
                // October to November boundary): days 10-30 and 10-31 are 0 and 1, 11-01 and 11-02 are 2 and 3.
                let digit = |i: usize| (self.data[p + i] - b'0') as usize;
                h = (digit(9) + digit(6)) * 24 + digit(11) * 10 + digit(12) - 18;
                (c, low, high, prev_prefix) = (cur[h], low.min(h), high.max(h), prefix);
            }
            // Safety: the line and the 128 bytes after its start are inside the input or its zero tail.
            let (ep, len, error, latency, next) = unsafe { line(d, p) };
            let e = self.id(d.wrapping_add(ep), len);
            let row = (e + 1) << 21 | (error as u32) << 20;
            if latency >= ESCAPE {
                let x = (latency as u64) << 32 | row as u64;
                self.long[h].lock().unwrap().push(x);
            } else {
                if c.0 == c.1 {
                    // A new chunk of CLAIM rows. A pooled region holds an older hour's bytes, and a row no line
                    // takes must read 0, so the chunk is zeroed first.
                    let i = self.claimed[h].fetch_add(CLAIM, Relaxed);
                    let at = self.region(h).wrapping_add(i);
                    // Safety: the chunk lies inside the region (stride holds a row per line plus a chunk per thread).
                    unsafe { at.write_bytes(0, CLAIM) };
                    c = (at, at.wrapping_add(CLAIM));
                }
                // Safety: c.0 < c.1, the claimed chunk's end.
                unsafe {
                    *c.0 = row | latency;
                    c.0 = c.0.add(1);
                }
            }
            p = next;
        }
        cur[h] = c;
        (low, high)
    }

    /// One of the run's `workers` threads; an hour is finished once the registered prefix holds a line `lag` hours later.
    fn work(&self, workers: usize, lag: usize) {
        let nblocks = self.data.len().div_ceil(BLOCK);
        let mut cur = [(std::ptr::null_mut(), std::ptr::null_mut()); HOURS];
        let mut s = self.state.lock().unwrap();
        while !s.late {
            if s.next_hour == HOURS {
                drop(s);
                // Every block is parsed: the mapping's page-table entries are dropped in shared 4 MiB steps, so the
                // exit has fewer to tear down (a-12: output closed -> process reaped 1.9-2.0 -> 0.8-1.3 ms).
                let steps = self.data.chunks(4 << 20);
                while let Some(step) = steps.clone().nth(self.released.fetch_add(1, Relaxed)) {
                    // Safety: step lies inside the mapping, which nothing reads now; 4 is MADV_DONTNEED.
                    unsafe { madvise(step.as_ptr(), step.len(), 4) };
                }
                return;
            } else if s.watermark == nblocks || s.next_hour + lag <= s.prefix_max {
                let h = s.next_hour;
                s.next_hour += 1;
                drop(s);
                self.ready.notify_all();
                // Hour h's text is handed over; every hour that is ready is placed in order, then written at its
                // offset, so threads write at once. Each region is pooled once its text is out, and the one pooled
                // before it goes back to the kernel (MADV_DONTNEED 4; length 0 when there was none).
                let text = self.finish(h);
                s = self.state.lock().unwrap();
                let st = &mut *s;
                st.parts.insert(h, text);
                let mut ready = Vec::new();
                while let Some(t) = st.parts.remove(&st.placed) {
                    ready.push((st.placed, st.offset, t));
                    (st.placed, st.offset) = (st.placed + 1, st.offset + t.len() as u64);
                }
                drop(s);
                for (h, offset, t) in ready {
                    self.file.write_all_at(t, offset).expect("write output");
                    let new = self.regions[h].load(Relaxed);
                    let r = replace(&mut self.pool.lock().unwrap().0, new);
                    // Safety: the region is page-aligned and nothing reads it any more.
                    unsafe { madvise(r as *const u8, self.stride * 4 * (r > 0) as usize, 4) };
                }
            } else if s.next_block < nblocks && s.next_block < s.watermark + 2 * workers {
                let b = s.next_block;
                s.next_block += 1;
                drop(s);
                let (low, high) = self.parse((self.align(b), self.align(b + 1)), &mut cur);
                // Block b is registered; a line of an hour already claimed marks the run late.
                s = self.state.lock().unwrap();
                let st = &mut *s;
                st.late |= low < st.next_hour;
                st.top.insert(b, high);
                while let Some(t) = st.top.remove(&st.watermark) {
                    (st.prefix_max, st.watermark) = (st.prefix_max.max(t), st.watermark + 1);
                }
                drop(s);
                self.ready.notify_all();
            } else {
                s = self.ready.wait(s).unwrap();
                continue;
            }
            s = self.state.lock().unwrap();
        }
    }

    /// Hour h's output lines in endpoint byte order (spec 4). The rows are counted per endpoint, sorted by latency in two
    /// counting passes and dealt to each endpoint's span in byte order, so every span is sorted and p50, p95 and p99 are
    /// read off by rank. The text is written over the rows, in the hour's region.
    fn finish(&self, h: usize) -> &'static [u8] {
        let (order, names) = self.order();
        let claimed = self.claimed[h].load(Relaxed);
        // Safety: the region is stride rows; its rows are all written (their blocks are registered) and no other thread
        // touches it until it goes to the pool.
        let region = unsafe { from_raw_parts_mut(self.region(h), self.stride) };
        let (head, values) = region.split_at_mut(claimed.max(SPARE));
        let mut long = take(&mut *self.long[h].lock().unwrap());
        let rows = &mut head[..claimed];
        let mut count = [0u32; 2 * IDS]; // by (id + 1) << 1 | 5xx; a free slot counts at 0
        let (mut low, mut high) = ([0u32; 512], [0u32; 2048]); // by latency bits 0-8, 9-19
        for &r in rows.iter() {
            count[(r >> 20) as usize] += 1;
            low[(r & 511) as usize] += 1;
            high[(r >> 9 & 2047) as usize] += 1;
        }
        long.iter()
            .for_each(|&p| count[(p as u32 >> 20) as usize] += 1);
        // By id + 1; start[0] is a trash slot past every span, where the free slots go.
        let (mut start, mut sum) = ([0u32; IDS], 0);
        for &id in &order {
            let k = id as usize + 1;
            (start[k], sum) = (sum, sum + count[2 * k] + count[2 * k + 1]);
        }
        start[0] = sum;
        let mut next = start;
        // Sorted by latency (values is scratch here), then dealt to the spans in that order: each span is sorted, the
        // long latencies (all ESCAPE or more) last, sorted among themselves.
        deal(rows, values, &mut low, 0);
        deal(&values[..claimed], rows, &mut high, 9);
        long.sort_unstable();
        let short = rows.iter().map(|&r| (r >> 21, r & ESCAPE));
        let long = long.iter().map(|&p| (p as u32 >> 21, (p >> 32) as u32));
        short.chain(long).for_each(|(k, v)| {
            let k = k as usize;
            // Safety: an endpoint's slots run from its start to its start plus its count, all below sum, and the trash
            // slot is sum, all inside values (a row per line past head).
            unsafe { *values.get_unchecked_mut(next[k] as usize) = v };
            next[k] += (k != 0) as u32;
        });

        let (day, hour) = (DAYS[(h + 18) / 24], (h + 18) % 24);
        let label = format!("{day}T{hour:02}:00:00Z{:12}", ""); // 32 bytes
        let text = head.as_mut_ptr() as *mut u8;
        let mut w = text;
        for &id in &order {
            let k = id as usize + 1;
            let (n, errors) = (count[2 * k] + count[2 * k + 1], count[2 * k + 1]);
            if n == 0 {
                continue;
            }
            // Spec 3.4: 0-based index of rank ceil(p * n / 100).
            let at = |p: usize| values[start[k] as usize + (p * n as usize + 99) / 100 - 1];
            // Spec 3.6: error_ppm = floor(errors * 1000000 / count), in 64 bits.
            let ppm = (errors as u64 * 1_000_000 / n as u64) as u32;
            // Safety: the names hold every id in order and never move or change (see Job).
            let name = unsafe { &*names.add(id as usize) };
            // Safety: head holds at least SPARE rows (512 KiB), past every endpoint's longest line and the bytes the
            // label, name and number stores run over it; each copy reads only its own source's bytes (label 32, name
            // 128).
            unsafe {
                copy(label.as_ptr(), w, 32);
                copy(name.as_ptr(), w.add(21), 128);
                w = w.add(21 + name[127] as usize);
                for v in [n, at(50), at(95), at(99), errors, ppm] {
                    w = put(w, v);
                }
                *w = b'\n';
                w = w.add(1);
            }
        }
        // Safety: every byte from text to w is written above, and the region is never unmapped.
        unsafe { from_raw_parts(text, w.offset_from(text) as usize) }
    }
}

/// v < 10^8 as eight digit bytes (0 to 9), the most significant in the low byte: the 4-digit halves, then the 2-digit
/// and 1-digit parts, side by side in one u64, divided by multiplying (x * 10486 >> 20 is x / 100 below 10^4, and
/// x * 103 >> 10 is x / 10 below 100).
fn decimal8(v: u32) -> u64 {
    let x = (v / 10_000) as u64 | ((v % 10_000) as u64) << 32;
    let hundreds = (x * 10_486 >> 20) & 0x7f_0000_007f;
    let x = hundreds | (x - 100 * hundreds) << 16;
    let tens = (x * 103 >> 10) & 0x000f_000f_000f_000f;
    tens | (x - 10 * tens) << 8
}

/// Writes a space and v in decimal at w and returns the end. The digits are made at once and the leading zeros
/// shifted out, so every width below 10^8 runs the same instructions; the 8- or 16-byte store passes the number's end
/// by up to 7 bytes, which the next write covers. Safety: w..w + 17 must be writable.
unsafe fn put(w: *mut u8, v: u32) -> *mut u8 {
    *w = b' ';
    if v < 100_000_000 {
        // The top bit keeps the last digit when v is 0.
        let d = decimal8(v);
        let lead = (d | 1 << 63).trailing_zeros() / 8;
        (w.add(1) as *mut u64).write_unaligned((d | 0x3030_3030_3030_3030) >> (8 * lead));
        return w.add(9 - lead as usize);
    }
    let d = decimal8(v / 100_000_000) as u128 | (decimal8(v % 100_000_000) as u128) << 64;
    let digits = d | u128::from_ne_bytes([b'0'; 16]);
    let lead = (d | 1 << 127).trailing_zeros() / 8;
    (w.add(1) as *mut u128).write_unaligned(digits >> (8 * lead));
    w.add(17 - lead as usize)
}

/// One stable counting pass: src to dst in the order of the log2(N) bits at `shift`, whose counts are in hist.
fn deal<const N: usize>(src: &[u32], dst: &mut [u32], hist: &mut [u32; N], shift: u32) {
    let mut at = 0;
    for c in hist.iter_mut() {
        (*c, at) = (at, at + *c);
    }
    for &r in src {
        let b = &mut hist[(r >> shift) as usize & (N - 1)];
        // Safety: the counts add up to src.len(), which dst holds.
        unsafe { *dst.get_unchecked_mut(*b as usize) = r };
        *b += 1;
    }
}

/// One run over the input with `workers` threads; false when a line came after its hour was finished.
fn run(data: &'static [u8], file: &File, workers: usize, lag: usize) -> bool {
    // No transparent huge pages (PR_SET_THP_DISABLE 41): with THP on, an hour's first row would charge 2 MiB.
    unsafe { prctl(41, 1usize, 0usize, 0usize, 0usize) };
    // A row per line (spec 2: a line has at least 40 bytes) and one partly used chunk per thread, then as many for
    // the latencies and a trash slot; 1,024 rows are one 4 KiB page, so regions start on pages.
    let rows = data.len() / 40 + 1 + workers * CLAIM;
    let stride = (SPARE + 2 * rows + 1).next_multiple_of(1024);
    // PROT_READ | PROT_WRITE 3; MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE 0x4022.
    let base = unsafe { mmap(std::ptr::null_mut(), stride * HOURS * 4, 3, 0x4022, -1, 0) };
    let job = Job {
        data,
        file,
        state: Mutex::default(),
        ready: Condvar::default(),
        released: AtomicUsize::default(),
        base: base as *mut u32,
        stride,
        regions: [const { AtomicUsize::new(0) }; HOURS],
        pool: Mutex::default(),
        claimed: [const { AtomicUsize::new(0) }; HOURS],
        long: [const { Mutex::new(Vec::new()) }; HOURS],
        slots: [const { AtomicU64::new(0) }; SLOTS],
        names: Mutex::new((Vec::with_capacity(IDS), Vec::new())),
    };
    std::thread::scope(|s| {
        for _ in 1..workers {
            s.spawn(|| job.work(workers, lag));
        }
        job.work(workers, lag);
    });
    !job.state.into_inner().unwrap().late
}

fn main() {
    let file = File::open("/data/requests.log").expect("open input");
    let len = file.metadata().expect("stat input").len() as usize;
    let out = File::create("/out/summary.txt").expect("create output");
    // Map the file read-only over an anonymous reservation 4 KiB longer (PROT_READ 1; MAP_PRIVATE | MAP_ANONYMOUS
    // 0x22, then MAP_PRIVATE | MAP_FIXED 0x12), never unmapped, so the slice lives for the run and the bytes past the
    // input read as zeros: the kernel zero-fills the file's last page and the reservation is zeros. Then ask the
    // kernel to read the file ahead (MADV_WILLNEED 3), a hint whose result is ignored. An empty log (spec 3.7: an
    // empty summary) maps nothing over the reservation, which reads as no lines.
    let room = unsafe { mmap(std::ptr::null_mut(), len + 4096, 1, 0x22, -1, 0) };
    unsafe { mmap(room, len, 1, 0x12, file.as_raw_fd(), 0) };
    unsafe { madvise(room, len, 3) };
    let data: &'static [u8] = unsafe { from_raw_parts(room, len) };

    // Worker count. SD_WORKERS is a test-only override (rule 30: the hash runs force 1, 4, 12 and 24);
    // the board never sets it, so the scored run sizes itself from the CPUs it may use.
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    let var = std::env::var("SD_WORKERS").unwrap_or_default();
    let workers = var.trim().parse().unwrap_or(cpus).max(1);
    if !run(data, &out, workers, LAG) {
        // A line came after its hour was finished: again from an empty file, with no hour finished before every block
        // is registered, so the output is exact for any line order.
        out.set_len(0).expect("truncate output");
        run(data, &out, workers, HOURS);
    }
    // Everything is written; exit without freeing what the kernel reclaims anyway.
    std::process::exit(0);
}
