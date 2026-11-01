//! Track A: per (hour, endpoint) request count, nearest-rank p50/p95/p99 latency and 5xx rate.
//! The threads claim an mmap of /data/requests.log in 256 KiB blocks, in file order, and turn each block's lines into
//! per-hour records. An hour is finished (counted, selected, formatted) as soon as the registered prefix of the file holds
//! a line two hours later, while other threads keep parsing, and finished hours are written in hour order: no thread
//! waits for the whole scan, and only a few hours' records are alive at once.
//! Main tricks: line_fields (a-5), parse_latency (a-13), Table (a-10, a-40), percentiles (a-20), the pipeline (a-55).

use std::arch::x86_64::*;
use std::sync::atomic::{AtomicUsize, Ordering::Acquire, Ordering::Relaxed, Ordering::Release};
use std::sync::{Condvar, Mutex};
use std::{cell::UnsafeCell, collections::BTreeMap, fs::File, io::prelude::*, os::fd::AsRawFd};

// Spec 2 grammar: ts (27 bytes), SP, method (3 to 7 bytes), SP: the endpoint starts at byte 32 to 36 of a line.
const ENDPOINT_MIN: usize = 32;
// Spec 2 grammar: ts = "YYYY-MM-DDTHH..."; bytes 0..13 name the UTC hour.
const HOUR_PREFIX: usize = 13;
// Spec 2: every timestamp lies in the same 72 hours for every seed and scale, 2026-10-30T18:00Z to 2026-11-02T17:59Z.
const HOURS: usize = 72;
const DAYS: [&str; 4] = ["2026-10-30", "2026-10-31", "2026-11-01", "2026-11-02"];
// Spec 2: exactly 1,842 distinct endpoints per seed, so global ids stay below 2,048 (11 bits).
const IDS: usize = 2048;
const BLOCK: usize = 256 << 10;
// An hour is finished once the registered prefix of the file holds a line LAG hours later. Spec 2 promises no bound on
// how far a line is out of order (generated files: at most one hour behind the latest hour before it), so a line that
// comes after its hour was finished is detected, and main repeats the run with no early finishing: exact for any order.
const LAG: usize = 2;

unsafe extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn madvise(addr: *const u8, len: usize, advice: i32) -> i32;
    fn mallopt(param: i32, value: i32) -> i32;
}

/// The first `byte` at or after `from`, or the input's end.
fn find(data: &[u8], from: usize, byte: u8) -> usize {
    let pos = data[from..].iter().position(|&b| b == byte);
    pos.map_or(data.len(), |off| from + off)
}

/// Hours since 2026-10-30T18 of the line at p, from "YYYY-MM-DDTHH" (spec 2: the window crosses two midnights and the
/// October to November boundary): days 10-30 and 10-31 are 0 and 1, 11-01 and 11-02 are 2 and 3.
fn hour_index(p: &[u8]) -> usize {
    let d = |i: usize| (p[i] - b'0') as usize;
    (d(9) + d(6)) * 24 + d(11) * 10 + d(12) - 18
}

/// Byte mask of `byte` in data[i..i + 32], one bit per byte.
/// Safety: the caller keeps i + 32 <= data.len().
unsafe fn mask32(data: &[u8], i: usize, byte: u8) -> u32 {
    // AVX2 is enabled at compile time (x86-64-v3, .cargo/config.toml).
    let v = unsafe { _mm256_loadu_si256(data.as_ptr().add(i) as *const __m256i) };
    unsafe { _mm256_movemask_epi8(_mm256_cmpeq_epi8(v, _mm256_set1_epi8(byte as i8))) as u32 }
}

/// The endpoint's first byte and the LF of the line starting at p. The method holds no '/', so the first '/'
/// from byte 32 opens the endpoint; one 32-byte load finds it and, for most lines, the LF too.
/// Measured on this VM (seed 1 full, 4 workers; a-5): scan 108 -> 79 ns per line against byte-by-byte searches
/// (an AVX2 LF search alone gave 95, adding a SWAR '/' search 86).
fn line_fields(data: &[u8], p: usize) -> (usize, usize) {
    let mut i = p + ENDPOINT_MIN;
    if i + 64 > data.len() {
        // Near the end of the input, where a 64-byte load would overrun: byte by byte.
        let ep = find(data, i, b'/');
        return (ep, find(data, ep, b'\n'));
    }
    // One 64-byte window from byte 32 holds the '/' and, for lines of up to 96 bytes (about 90 % of seed 1's), the
    // LF, so most lines take no loop turn (a-40a). Cachegrind, 1 worker, against one 32-byte load and a loop: scan
    // 230.7 -> 218.2 instructions per line (seed 1), 232.4 -> 220.5 (seed 4242); conditional mispredicts -0.40 per
    // line (seed 1), -0.45 (seed 4242).
    // Safety: i + 64 <= data.len(), checked above.
    let ep = i + unsafe { mask32(data, i, b'/') }.trailing_zeros() as usize;
    let lf = unsafe { mask32(data, i, b'\n') as u64 | (mask32(data, i + 32, b'\n') as u64) << 32 };
    if lf != 0 {
        return (ep, i + lf.trailing_zeros() as usize);
    }
    i += 64;
    loop {
        if i + 32 > data.len() {
            return (ep, find(data, i, b'\n'));
        }
        let lf = unsafe { mask32(data, i, b'\n') }; // Safety: i + 32 <= data.len(), checked just above.
        if lf != 0 {
            return (ep, i + lf.trailing_zeros() as usize);
        }
        i += 32;
    }
}

/// The space before the latency and the latency's value, for the line ending at lf, with no per-digit loop.
/// One 16-byte load ends at the LF (spec 2: every line is at least 40 bytes, so it stays inside the line); the
/// highest space in it precedes the latency's 1 to 10 digits (spec 2). Lanes left of the digits are zeroed,
/// so the digits sit right-aligned in 16 lanes, and two multiply-add steps fold them into four 4-digit groups.
/// Spec 2 rules out the malformed cases (no space, over 10 digits, a non-digit, a value over u32), so none is checked.
/// Measured on this VM (cachegrind, seed 7 sample, 1 worker; a-13): conditional mispredicts 6.86 -> 5.32 per line,
/// instructions 726 -> 693 per line, against walking back to the space and a per-digit checked loop.
fn parse_latency(data: &[u8], lf: usize) -> (usize, u32) {
    let v = &data[lf - 16..lf];
    // Safety: v is 16 readable bytes; SSE2 and SSSE3 are part of the x86-64-v3 baseline.
    unsafe {
        let x = _mm_loadu_si128(v.as_ptr() as *const __m128i);
        let spaces = _mm_movemask_epi8(_mm_cmpeq_epi8(x, _mm_set1_epi8(b' ' as i8))) as u32;
        let sp = 31 - spaces.leading_zeros();
        let lanes = _mm_setr_epi8(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15);
        let keep = _mm_cmpgt_epi8(lanes, _mm_set1_epi8(sp as i8));
        let digits = _mm_and_si128(_mm_sub_epi8(x, _mm_set1_epi8(b'0' as i8)), keep);
        let tens = _mm_setr_epi8(10, 1, 10, 1, 10, 1, 10, 1, 10, 1, 10, 1, 10, 1, 10, 1);
        let pairs = _mm_maddubs_epi16(digits, tens);
        let hundreds = _mm_setr_epi16(100, 1, 100, 1, 100, 1, 100, 1);
        let q: [u32; 4] = std::mem::transmute(_mm_madd_epi16(pairs, hundreds));
        // At most 10 digits, so lane 0 (bytes 0..4) is always zero.
        let value = q[1] as u64 * 100_000_000 + q[2] as u64 * 10_000 + q[3] as u64;
        (lf - 16 + sp as usize, value as u32)
    }
}

/// 32 bytes at p, zeroed from byte n on (n <= 32): one 32-byte chunk of an endpoint. Spec 2 endpoints hold no zero
/// byte, so a chunk zeroed past the endpoint's end also tells the end, and equal chunks mean equal endpoints.
/// Safety: p..p + 32 must be readable.
unsafe fn chunk(p: *const u8, n: usize) -> __m256i {
    const MASK: &[u8; 64] = b"\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
    unsafe {
        let v = _mm256_loadu_si256(p as _);
        _mm256_and_si256(v, _mm256_loadu_si256(MASK.as_ptr().add(32 - n) as _))
    }
}

fn same(a: __m256i, b: __m256i) -> bool {
    // Safety: AVX2 is enabled at compile time (x86-64-v3).
    unsafe { _mm256_movemask_epi8(_mm256_cmpeq_epi8(a, b)) == -1 }
}

/// Hash of an endpoint's first chunk: its two 16-byte halves xor-ed, then two CRC32 steps (SSE4.2, x86-64-v3).
fn key_hash(v: __m256i) -> u64 {
    // Safety: AVX2 and SSE4.2 are enabled at compile time (x86-64-v3).
    unsafe {
        let x = _mm_xor_si128(_mm256_castsi256_si128(v), _mm256_extracti128_si256::<1>(v));
        let h = _mm_crc32_u64(0, _mm_cvtsi128_si64(x) as u64);
        _mm_crc32_u64(h, _mm_extract_epi64::<1>(x) as u64)
    }
}

/// One endpoint, zero-padded to 128 bytes (spec 2: 1 to 125 bytes, never a zero byte), so a lookup compares it in
/// whole 32-byte chunks; byte 127 holds its length. Exactly two cache lines, so the confirm of an endpoint up to 64
/// bytes reads one line. On the box (perf, 12 workers) the confirm took ~18 pct of all cycles when each name was an
/// OnceLock<Name> (a 144-byte stride, its Once state on another line). Effect: PERF.md a-55.
#[repr(C, align(64))]
struct Name([u8; 128]);

impl Name {
    fn bytes(&self) -> &[u8] {
        &self.0[..self.0[127] as usize]
    }
}

/// The endpoints any thread has met, one global id each. Only the holder of `adding` writes a name, names[published],
/// and then moves `published` past it (Release); every reader takes ids below `published` (Acquire), so a name is
/// written before any read of it and never again. `sorted` holds ids 0..sorted.len() in byte order (spec 4); finishing
/// threads sort the newly published ids in, so a parsing thread's lock covers only the id itself. The box split (a-55
/// probe) showed 1.4-2.1 ms per thread inside misses when the sorted insert ran under that lock: 1,842 inserts, most in
/// the first milliseconds, while all 12 threads meet the same endpoints. Effect: PERF.md a-55.
#[derive(Default)]
struct Dict {
    names: Vec<UnsafeCell<Name>>,
    published: AtomicUsize,
    adding: Mutex<()>,
    sorted: Mutex<Vec<u16>>,
}

// Safety: names are written only as described above (one writer, before the Release that publishes them).
unsafe impl Sync for Dict {}

impl Dict {
    /// Name `id`; the caller took id below `published` (Acquire).
    fn name(&self, id: usize) -> &Name {
        // Safety: a published name is written before its Release and never written again.
        unsafe { &*self.names[id].get() }
    }

    /// Every published id, in byte order: the ids published since the last call are appended and sorted in (the sort
    /// finds the sorted prefix as one run, so it costs about one merge).
    fn order(&self) -> Vec<u16> {
        let n = self.published.load(Acquire);
        let mut sorted = self.sorted.lock().unwrap();
        if sorted.len() < n {
            let len = sorted.len() as u16;
            sorted.extend(len..n as u16);
            sorted.sort_by_key(|&id| self.name(id as usize).bytes());
        }
        sorted.clone()
    }
}

/// One thread's open-addressing (linear probing) view of the dictionary: a slot is length << 24 | global id + 1, 0 when
/// empty, and 4,096 slots stay at most half full (spec 2: 1,842 endpoints), so the table never grows. A hit needs the
/// slot's length and the line's first two chunks equal to the stored name's, and an endpoint over 64 bytes also compares
/// its other chunks, so there is no byte compare call and no length branch up to 64 bytes (a-40b: scan 218.2 -> 189.1
/// instructions per line on seed 1 against a four-multiply hash and bcmp confirm, cachegrind, 1 worker).
struct Table {
    slots: Vec<u32>,
    synced: usize,
}

impl Table {
    fn put(&mut self, dict: &Dict, id: usize) {
        let name = dict.name(id);
        // Safety: the name is 128 readable bytes; zero past its end, its first chunk equals the masked first chunk
        // of any line holding this endpoint, so both hash alike.
        let mut i = key_hash(unsafe { chunk(name.0.as_ptr(), 32) }) as usize & (self.slots.len() - 1);
        while self.slots[i] != 0 {
            i = (i + 1) & (self.slots.len() - 1);
        }
        self.slots[i] = id as u32 + 1 | (name.bytes().len() as u32) << 24;
    }

    /// Adds the names other threads published since this one last looked.
    fn sync(&mut self, dict: &Dict) {
        let end = dict.published.load(Acquire);
        for id in self.synced..end {
            self.put(dict, id);
        }
        self.synced = end;
    }

    /// The global id of the endpoint at p of length len, whose first two chunks are v; p + 128 must be readable.
    #[inline(always)]
    fn find(&self, dict: &Dict, v: [__m256i; 2], p: *const u8, len: usize) -> Option<u32> {
        let mask = self.slots.len() - 1;
        let mut i = key_hash(v[0]) as usize & mask;
        // The probe loop calls nothing, so the key vectors stay in registers.
        while self.slots[i] != 0 {
            let id = (self.slots[i] & 0xFF_FFFF) as usize - 1;
            // Safety: a slot holds only published ids, whose names are set; a name is 128 bytes, p + 128 is readable, and
            // equal lengths keep every chunk compare inside both endpoints' padded bytes (the last chunk is masked on
            // both sides, so the stored length in byte 127 is not compared).
            let hit = self.slots[i] >> 24 == len as u32 && unsafe {
                let q = dict.names.get_unchecked(id).get() as *const u8;
                let load = |k: usize| _mm256_loadu_si256(q.add(k) as _);
                same(v[0], load(0))
                    & same(v[1], load(32))
                    & (len <= 64 || same(chunk(p.add(64), (len - 64).min(32)), load(64)))
                    & (len <= 96 || same(chunk(p.add(96), len - 96), chunk(q.add(96), len - 96)))
            };
            if hit {
                return Some(id as u32);
            }
            i = (i + 1) & mask;
        }
        None
    }

    /// The global id of the endpoint b[s..s + len]; the caller keeps s + 128 within b.
    #[inline(always)]
    fn endpoint(&mut self, dict: &Dict, b: &[u8], s: usize, len: usize) -> u32 {
        // Safety: s + 128 <= b.len(), and an endpoint has at most 125 bytes (spec 2), so both chunks are readable.
        let p = b.as_ptr().wrapping_add(s);
        let v = unsafe { [chunk(p, len.min(32)), chunk(p.add(32), len.clamp(32, 64) - 32)] };
        match self.find(dict, v, p, len) {
            Some(id) => id,
            None => self.miss(dict, v, p, &b[s..s + len]),
        }
    }

    /// An endpoint this thread has not met: first the names other threads published, then, under the lock, a new id.
    #[cold]
    fn miss(&mut self, dict: &Dict, v: [__m256i; 2], p: *const u8, ep: &[u8]) -> u32 {
        self.sync(dict);
        if let Some(id) = self.find(dict, v, p, ep.len()) {
            return id;
        }
        let adding = dict.adding.lock().unwrap();
        self.sync(dict);
        if let Some(id) = self.find(dict, v, p, ep.len()) {
            return id;
        }
        let id = self.synced; // == published, which only the lock holder moves
        // Safety: only the holder of `adding` writes, and no thread reads names[id] before the Release below.
        let name = unsafe { &mut (*dict.names[id].get()).0 };
        name[..ep.len()].copy_from_slice(ep);
        name[127] = ep.len() as u8;
        dict.published.store(id + 1, Release);
        drop(adding);
        self.sync(dict);
        id as u32
    }
}

/// The lines starting in data[start..end], as one record vector per hour met. A record is the latency in the low
/// 32 bits and, above them, the endpoint's global id | 5xx << 11, so the hour's counts come from its records.
fn parse(data: &[u8], start: usize, end: usize, table: &mut Table, dict: &Dict) -> Vec<(usize, Vec<u64>)> {
    let mut runs: Vec<(usize, Vec<u64>)> = Vec::new();
    let (mut prev_prefix, mut at, mut p) = ((u64::MAX, u64::MAX), 0, start);
    while p < end {
        // Lines usually share the previous line's hour, so compare its 13-byte prefix first, as two overlapping
        // u64 loads (bytes 0..8 and 5..13) rather than a slice compare.
        let word = |i: usize| u64::from_le_bytes(data[i..i + 8].try_into().unwrap());
        let prefix = (word(p), word(p + HOUR_PREFIX - 8));
        if prefix != prev_prefix {
            let h = hour_index(&data[p..]);
            at = match runs.iter().position(|r| r.0 == h) {
                Some(i) => i,
                None => {
                    // Spec 2: every line is at least 40 bytes, so the block holds at most this many.
                    runs.push((h, Vec::with_capacity((end - start) / 40 + 1)));
                    runs.len() - 1
                }
            };
            prev_prefix = prefix;
        }
        let (ep_start, lf) = line_fields(data, p);
        let (sp, latency) = parse_latency(data, lf);
        // Spec 2 grammar: status is three digits, 100 to 599, followed by SP; it is a 5xx error exactly when its
        // first digit is '5', so only that byte is read.
        let error = (data[sp - 3] == b'5') as u64;
        let len = sp - 4 - ep_start;
        let e = match data.get(..ep_start + 128) {
            Some(b) => table.endpoint(dict, b, ep_start, len),
            None => {
                // The input's last 128 bytes: the endpoint is copied into a zeroed buffer, so no load passes the end.
                let mut buf = [0u8; 128];
                buf[..len].copy_from_slice(&data[ep_start..sp - 4]);
                table.endpoint(dict, &buf, 0, len)
            }
        };
        runs[at].1.push(latency as u64 | (e as u64 | error << 11) << 32);
        p = lf + 1;
    }
    runs
}

/// Appends v in decimal, one digit per step from the low end. Against a-19's two-digit table (a 200-byte table
/// and an odd-digit tail, 17 counted lines more), cachegrind (seed 7 sample, 1 worker, 66,023 output lines):
/// 176.1 M -> 183.5 M instructions, +112 per output line, and 1.510 M -> 1.476 M mispredicts. On seed 1 full
/// that is about 14 M instructions, about 0.1 ms over 12 threads: under the 17 lines' price (1 ms per 67 lines).
fn push_u64(out: &mut Vec<u8>, mut v: u64) {
    let (mut buf, mut i) = ([0u8; 20], 20);
    loop {
        i -= 1;
        (buf[i], v) = (b'0' + (v % 10) as u8, v / 10);
        if v == 0 {
            return out.extend_from_slice(&buf[i..]);
        }
    }
}

/// A group of at most this many latencies is sorted outright: below it a sort is cheaper than three selects. The
/// cut depends only on the group's own count. Measured on this VM (seed 1 full, select time summed over threads; a-20):
/// 16 lost at 4 and 12 workers; 32 and 64 measured alike, and 32 is kept.
const SORT_MAX: usize = 32;

/// Exact nearest-rank p50, p95, p99 of an unsorted slice: a small group is sorted; a larger one is selected in
/// ascending rank order, each rank inside the part right of the one before (everything there is no smaller):
/// p50 on all n values, p95 on about n/2, p99 on about n/20, about 1.55n of select work against about 2.9n when
/// the highest rank went first and each lower one searched the part left of it.
/// Measured on this VM, seed 1 full, select time summed over threads, against that highest-first order (a-17):
/// 25.3 -> 19.5 ms at 1 worker, 29.8 -> 21.7 at 4, 37.6 -> 26.5 at 12; the shuffled file 35.3 -> 28.2 ms at 4.
/// Cachegrind (seed 7 sample, 1 worker): 3,106 -> 2,857 instructions per output line.
fn percentiles(lat: &mut [u32]) -> [u32; 3] {
    // Spec 3.4: 0-based index of rank ceil(p * n / 100).
    let idx = |p: usize| (p * lat.len() + 99) / 100 - 1;
    let (i50, i95, i99) = (idx(50), idx(95), idx(99));
    if lat.len() <= SORT_MAX {
        lat.sort_unstable();
        return [lat[i50], lat[i95], lat[i99]];
    }
    // Above SORT_MAX the three ranks are distinct: 0.95n - 0.5n and 0.99n - 0.95n both exceed 1, so their ceilings differ.
    let (_, &mut v50, above) = lat.select_nth_unstable(i50);
    let v95 = *above.select_nth_unstable(i95 - i50 - 1).1;
    let v99 = *lat[i95 + 1..].select_nth_unstable(i99 - i95 - 1).1;
    [v50, v95, v99]
}

/// The run's shared progress, under one lock.
#[derive(Default)]
struct State {
    next_block: usize,
    top: BTreeMap<usize, usize>, // per registered block past the watermark: its last hour
    watermark: usize,            // every block below it is registered
    prefix_max: usize,           // the last hour in the blocks below the watermark
    runs: BTreeMap<usize, Vec<Vec<u64>>>, // per hour: the registered blocks' records
    next_hour: usize,            // every hour below it is claimed for finishing
    late: bool,                  // a block brought lines of an hour already claimed
}

impl State {
    /// Hands block b's records to their hours; a record for an hour already claimed marks the run late.
    fn register(&mut self, b: usize, runs: Vec<(usize, Vec<u64>)>) {
        self.top.insert(b, runs.iter().map(|r| r.0).max().unwrap_or(0));
        for (h, recs) in runs {
            // A late record is kept like the others: the run is repeated anyway.
            self.late |= h < self.next_hour;
            self.runs.entry(h).or_default().push(recs);
        }
        while let Some(t) = self.top.remove(&self.watermark) {
            self.prefix_max = self.prefix_max.max(t);
            self.watermark += 1;
        }
    }
}

/// Finished hours waiting for the ones before them, and how many are written.
#[derive(Default)]
struct Out {
    parts: BTreeMap<usize, Vec<u8>>,
    written: usize,
    writing: bool,
}

/// One run over the input (a-55). Each thread takes, in this order: an hour that is safe to finish, else the next
/// block, at most two per thread past the oldest unregistered one, so few records wait; else it sleeps until a block
/// is registered or an hour claimed; once all hours are claimed it releases the mapping and ends. Unlike a-37 (static
/// ranges, finishing on the scan threads, then a barrier and a serial merge), no thread idles while work is left, and
/// the finishing fills the gaps. Measured effect: PERF.md a-55.
struct Job<'a> {
    data: &'static [u8],
    dict: &'a Dict,
    file: &'a File,
    state: Mutex<State>,
    ready: Condvar,
    out: Mutex<Out>,
    released: AtomicUsize,
}

impl Job<'_> {
    /// Block b's first line start: the first at or after b * BLOCK (spec 2: the last line ends with an LF).
    fn align(&self, b: usize) -> usize {
        match (b * BLOCK).min(self.data.len()) {
            0 => 0,
            n => find(self.data, n - 1, b'\n') + 1,
        }
    }

    /// One of the run's `workers` threads; an hour is finished once the registered prefix holds a line `lag` hours later.
    fn work(&self, workers: usize, lag: usize) {
        let nblocks = self.data.len().div_ceil(BLOCK);
        let slots = vec![0; 2 * IDS];
        let mut table = Table { slots, synced: 0 };
        let mut scratch = Vec::new();
        let mut s = self.state.lock().unwrap();
        while !s.late {
            if s.next_hour == HOURS {
                drop(s);
                return self.release();
            } else if s.watermark == nblocks || s.next_hour + lag <= s.prefix_max {
                let h = s.next_hour;
                s.next_hour += 1;
                let runs = s.runs.remove(&h).unwrap_or_default();
                drop(s);
                self.ready.notify_all();
                self.emit(h, self.finish(h, runs, &mut scratch));
            } else if s.next_block < nblocks && s.next_block < s.watermark + 2 * workers {
                let b = s.next_block;
                s.next_block += 1;
                drop(s);
                let start = self.align(b);
                let runs = parse(self.data, start, self.align(b + 1), &mut table, self.dict);
                self.state.lock().unwrap().register(b, runs);
                self.ready.notify_all();
            } else {
                s = self.ready.wait(s).unwrap();
                continue;
            }
            s = self.state.lock().unwrap();
        }
    }

    /// Hour h's output lines in endpoint byte order (spec 4): the records counted per (id, 5xx), then a counting sort
    /// in byte order puts each endpoint's latencies side by side in scratch, with no comparison sort.
    fn finish(&self, h: usize, runs: Vec<Vec<u64>>, scratch: &mut Vec<u32>) -> Vec<u8> {
        let order = self.dict.order();
        let mut count = [0u32; 2 * IDS]; // by id | 5xx << 11
        for &x in runs.iter().flatten() {
            count[(x >> 32) as usize] += 1;
        }
        let (mut start, mut total) = ([0u32; IDS], 0);
        for &id in &order {
            start[id as usize] = total;
            total += count[id as usize] + count[id as usize | IDS];
        }
        // resize keeps the scratch capacity across hours; every slot below total is written by the placement.
        scratch.resize(total as usize, 0);
        let mut next = start;
        for &x in runs.iter().flatten() {
            let id = (x >> 32) as usize & (IDS - 1);
            scratch[next[id] as usize] = x as u32;
            next[id] += 1;
        }
        drop(runs); // every latency is placed: the records go back before the formatting
        let t = h + 18;
        let label = format!("{}T{:02}:00:00Z ", DAYS[t / 24], t % 24);
        // Reserved once at the most a line can take (spec 2: endpoints of at most 125 bytes; six numbers under 2^64), so
        // it never doubles; above the mmap threshold (main), only the pages written are charged.
        let mut buf = Vec::with_capacity(order.len() * (label.len() + 125 + 6 * 21 + 1));
        for &id in &order {
            let (n, errors) = (count[id as usize] + count[id as usize | IDS], count[id as usize | IDS] as u64);
            if n == 0 {
                continue;
            }
            let lat = &mut scratch[start[id as usize] as usize..][..n as usize];
            let [p50, p95, p99] = percentiles(lat).map(u64::from);
            buf.extend_from_slice(label.as_bytes());
            buf.extend_from_slice(self.dict.name(id as usize).bytes());
            // Spec 3.6: error_ppm = floor(errors * 1000000 / count), in 64 bits.
            for v in [n as u64, p50, p95, p99, errors, errors * 1_000_000 / n as u64] {
                buf.push(b' ');
                push_u64(&mut buf, v);
            }
            buf.push(b'\n');
        }
        buf
    }

    /// Hands over hour h's text; a thread that finds no write in progress writes every hour that is ready, in order.
    fn emit(&self, h: usize, text: Vec<u8>) {
        let mut o = self.out.lock().unwrap();
        o.parts.insert(h, text);
        if std::mem::replace(&mut o.writing, true) {
            return;
        }
        loop {
            let w = o.written;
            let Some(buf) = o.parts.remove(&w) else { break };
            drop(o);
            let mut file = self.file;
            file.write_all(&buf).expect("write output");
            drop(buf);
            o = self.out.lock().unwrap();
            o.written += 1;
        }
        o.writing = false;
    }

    /// Every block is parsed: the mapping's page-table entries are dropped in shared 4 MiB steps, so the exit has fewer
    /// to tear down (a-12: output closed -> process reaped 1.9-2.0 -> 0.8-1.3 ms). Nothing reads the mapping any more.
    fn release(&self) {
        let steps = self.data.chunks(4 << 20);
        while let Some(step) = steps.clone().nth(self.released.fetch_add(1, Relaxed)) {
            // Safety: step lies inside the mapping, which nothing writes or reads now; 4 is MADV_DONTNEED.
            unsafe { madvise(step.as_ptr(), step.len(), 4) };
        }
    }
}

/// One run over the input with `workers` threads; false when a line came after its hour was finished.
fn run(data: &'static [u8], dict: &Dict, file: &File, workers: usize, lag: usize) -> bool {
    let job = Job {
        data,
        dict,
        file,
        state: Mutex::default(),
        ready: Condvar::default(),
        out: Mutex::default(),
        released: AtomicUsize::default(),
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
    // Pin glibc's mmap threshold (M_MMAP_THRESHOLD, -3) at its 128 KiB default. Left dynamic, freeing the first
    // large buffer raises it to 1 MiB, so the output buffers come from the heap and their freed pages stay charged
    // while the output file grows. Measured (this VM, 12 workers, peak of RssAnon + output size; a-11): 22.4 -> 19.8 MB median.
    unsafe { mallopt(-3, 128 * 1024) };
    // One malloc arena (M_ARENA_MAX, -8) for every thread: a block's record vectors are freed by the thread that
    // finishes their hour, so with an arena per thread each arena keeps its own high-water mark of freed chunks.
    // With one, freed chunks are reused by whichever thread allocates next (a few allocations per block and per hour,
    // so the shared arena's lock is rarely contended). Measured effect: PERF.md a-55.
    unsafe { mallopt(-8, 1) };
    let file = File::open("/data/requests.log").expect("open input");
    let len = file.metadata().expect("stat input").len() as usize;
    let mut out = File::create("/out/summary.txt").expect("create output");
    if len == 0 {
        // Spec 3.7: an empty log gives an empty summary; mmap of length 0 would fail.
        return;
    }
    // Map the whole file read-only (PROT_READ 1, MAP_PRIVATE 2), never unmapped, so the slice lives for the run;
    // then ask the kernel to read the whole range ahead (MADV_WILLNEED 3), a hint whose result is ignored.
    let p = unsafe { mmap(std::ptr::null_mut(), len, 1, 2, file.as_raw_fd(), 0) };
    assert!(p as isize != -1, "mmap failed");
    unsafe { madvise(p, len, 3) };
    let data: &'static [u8] = unsafe { std::slice::from_raw_parts(p, len) };

    // Worker count. SD_WORKERS is a test-only override (rule 30: the hash runs force 1, 4, 12 and 24);
    // the board never sets it, so the scored run sizes itself from the CPUs it may use.
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    let var = std::env::var("SD_WORKERS").unwrap_or_default();
    let workers = var.trim().parse().unwrap_or(cpus).max(1);
    let dict = Dict {
        names: (0..IDS).map(|_| UnsafeCell::new(Name([0; 128]))).collect(),
        ..Default::default()
    };
    if !run(data, &dict, &out, workers, LAG) {
        // A line came after its hour was finished: again from an empty file, with no hour finished before every block
        // is registered, so the output is exact for any line order.
        out.set_len(0).expect("truncate output");
        out.rewind().expect("rewind output");
        run(data, &dict, &out, workers, HOURS);
    }
    // Everything is written; exit without freeing what the kernel reclaims anyway.
    std::process::exit(0);
}
