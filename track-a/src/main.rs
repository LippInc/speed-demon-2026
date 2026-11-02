//! Track A: per (hour, endpoint) request count, nearest-rank p50/p95/p99 latency and 5xx rate.
//! The threads claim an mmap of /data/requests.log in 256 KiB blocks, in file order, and turn each block's lines into
//! per-hour records. An hour is finished (counted, selected, formatted) as soon as the registered prefix of the file holds
//! a line two hours later, while other threads keep parsing, and finished hours are written in hour order: no thread
//! waits for the whole scan, and only a few hours' records are alive at once.
//! Main tricks: parse and line (a-56, a-57), Table (a-10, a-40, a-57), finish (a-57), the pipeline (a-55).

use std::arch::x86_64::*;
use std::sync::atomic::{AtomicUsize, Ordering::Acquire, Ordering::Relaxed, Ordering::Release};
use std::sync::{Condvar, Mutex};
use std::{cell::UnsafeCell, collections::BTreeMap, fs::File, io::prelude::*, os::fd::AsRawFd};

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

/// Byte mask of `byte` in the 32 bytes at p, one bit per byte. Safety: p..p + 32 must be readable.
unsafe fn mask32(p: *const u8, byte: u8) -> u32 {
    // AVX2 is enabled at compile time (x86-64-v3).
    unsafe {
        _mm256_movemask_epi8(_mm256_cmpeq_epi8(
            _mm256_loadu_si256(p as _),
            _mm256_set1_epi8(byte as i8),
        )) as u32
    }
}

/// The value of 8 decimal digits, one per byte (0 to 9), the most significant in the low byte: two multiply-add steps
/// fold digit pairs, then 2-digit pairs, into two 4-digit halves.
fn digits8(v: u64) -> u64 {
    // Safety: SSE2 and SSSE3 are part of the x86-64-v3 baseline.
    let r = unsafe {
        let pairs = _mm_maddubs_epi16(_mm_cvtsi64_si128(v as i64), _mm_set1_epi16(0x010a));
        _mm_cvtsi128_si64(_mm_madd_epi16(pairs, _mm_set1_epi32(0x0001_0064))) as u64
    };
    (r & 0xffff_ffff) * 10_000 + (r >> 32)
}

/// The endpoint's start and length, the 5xx flag and the latency of the line from p to its LF at lf (spec 2:
/// "<timestamp> <method> <endpoint> <status> <latency>", the timestamp always 27 bytes). The space after the method
/// is found from the front (one 32-byte mask: a method has at most 7 bytes) and the one before the latency from the
/// back (one 16-byte mask: at most 10 digits); the 3-digit status sits just before it, its first digit '5' exactly for
/// a 5xx. Safety: d + p..d + p + 60 and d + lf - 16..d + lf must be readable.
#[inline(always)]
unsafe fn line(d: *const u8, p: usize, lf: usize) -> (usize, usize, u64, u32) {
    unsafe {
        let ep = p + 29 + mask32(d.add(p + 28), b' ').trailing_zeros() as usize;
        let tail = _mm_loadu_si128(d.add(lf - 16) as _);
        let spaces = _mm_movemask_epi8(_mm_cmpeq_epi8(tail, _mm_set1_epi8(b' ' as i8))) as u32;
        let sp = lf - 16 + (31 - spaces.leading_zeros()) as usize;
        let digits = lf - sp - 1;
        // The 8 digit bytes ending at `at`, those before the last `keep` zeroed (they are spaces or status digits).
        let last = |at: usize, keep: usize| {
            ((d.add(at - 8) as *const u64).read_unaligned() ^ 0x3030_3030_3030_3030)
                & u64::MAX << (8 * (8 - keep))
        };
        let mut latency = digits8(last(lf, digits.min(8)));
        if digits > 8 {
            latency += 100_000_000 * digits8(last(lf - 8, digits - 8));
        }
        (
            ep,
            sp - 4 - ep,
            (*d.add(sp - 3) == b'5') as u64,
            latency as u32,
        )
    }
}

/// Hash of the len-byte endpoint at p: CRC32 (SSE4.2) of its length, first 8 and last 8 bytes, which set apart the
/// endpoints spec 2 generates (two that share both ends differ in length) and cost no masked copy of the bytes.
/// Safety: p..p + 8 and, from 8 bytes on, p + len - 8..p + len are readable.
unsafe fn ep_hash(p: *const u8, len: usize) -> usize {
    unsafe {
        let first = _bzhi_u64((p as *const u64).read_unaligned(), (8 * len).min(64) as u32);
        let last = if len >= 8 {
            (p.add(len - 8) as *const u64).read_unaligned()
        } else {
            0
        };
        _mm_crc32_u64(_mm_crc32_u64(len as u64, first), last) as usize
    }
}

/// Whether the len bytes at p are the name at q: a 32-byte compare gives one bit per equal byte, and the low len bits
/// must all be set; the bytes past the endpoint are not looked at, so no masked copy is needed.
/// Safety: p..p + 128 and q..q + 128 are readable.
unsafe fn equal(p: *const u8, q: *const u8, len: usize) -> bool {
    let eq = |k: usize| unsafe {
        let a = _mm256_loadu_si256(p.add(k) as _);
        _mm256_movemask_epi8(_mm256_cmpeq_epi8(a, _mm256_loadu_si256(q.add(k) as _))) as u32 as u64
    };
    let want = |n: usize| unsafe { _bzhi_u64(!0, n.min(64) as u32) };
    let low = eq(0) | eq(32) << 32;
    if len <= 64 {
        !low & want(len) == 0
    } else {
        low == !0 && !(eq(64) | eq(96) << 32) & want(len - 64) == 0
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
        let mut s = self.sorted.lock().unwrap();
        let (m, n) = (s.len() as u16, self.published.load(Acquire) as u16);
        if m < n {
            s.extend(m..n);
            s.sort_by_key(|&id| self.name(id as usize).bytes());
        }
        s.clone()
    }
}

/// One thread's open-addressing (linear probing) view of the dictionary: a slot is length << 24 | global id + 1, 0 when
/// empty; 8,192 slots stay under a quarter full (spec 2: 1,842 endpoints), so a hit is nearly always the first slot and
/// the table never grows. A hit needs the slot's length and the line's bytes equal to the stored name's.
const SLOTS: usize = 8192;

struct Table {
    slots: Box<[u32; SLOTS]>,
    synced: usize,
}

impl Table {
    fn put(&mut self, dict: &Dict, id: usize) {
        let name = dict.name(id);
        // Safety: a name is 128 readable bytes and its length is at most 125.
        let mut i = unsafe { ep_hash(name.0.as_ptr(), name.bytes().len()) } & (SLOTS - 1);
        while self.slots[i] != 0 {
            i = (i + 1) & (SLOTS - 1);
        }
        self.slots[i] = id as u32 + 1 | (name.bytes().len() as u32) << 24;
    }

    /// Adds the names other threads published since this one last looked.
    fn sync(&mut self, dict: &Dict) {
        let end = dict.published.load(Acquire);
        (self.synced..end).for_each(|id| self.put(dict, id));
        self.synced = end;
    }

    /// The global id of the len-byte endpoint at p with hash `hash`; p + 128 must be readable.
    #[inline(always)]
    fn find(&self, dict: &Dict, p: *const u8, len: usize, hash: usize) -> Option<u32> {
        let mut i = hash & (SLOTS - 1);
        loop {
            let slot = self.slots[i];
            if slot == 0 {
                return None;
            }
            let id = (slot & 0xFF_FFFF) - 1;
            // Safety: a slot holds only published ids, whose names are set and 128 bytes long; p + 128 is readable.
            if slot >> 24 == len as u32
                && unsafe {
                    equal(
                        p,
                        dict.names.get_unchecked(id as usize).get() as *const u8,
                        len,
                    )
                }
            {
                return Some(id);
            }
            i = (i + 1) & (SLOTS - 1);
        }
    }

    /// The global id of the endpoint b[s..s + len]; the 128 bytes from s must be readable (main maps a zero tail).
    #[inline(always)]
    fn endpoint(&mut self, dict: &Dict, b: &[u8], s: usize, len: usize) -> u32 {
        let p = b.as_ptr().wrapping_add(s);
        // Safety: the endpoint is inside b, and so are its first and last 8 bytes from 8 bytes on.
        let hash = unsafe { ep_hash(p, len) };
        match self.find(dict, p, len, hash) {
            Some(id) => id,
            None => self.miss(dict, p, hash, &b[s..s + len]),
        }
    }

    /// An endpoint this thread has not met: first the names other threads published, then, under the lock, a new id.
    #[cold]
    fn miss(&mut self, dict: &Dict, p: *const u8, hash: usize, ep: &[u8]) -> u32 {
        self.sync(dict);
        if let Some(id) = self.find(dict, p, ep.len(), hash) {
            return id;
        }
        let adding = dict.adding.lock().unwrap();
        self.sync(dict);
        if let Some(id) = self.find(dict, p, ep.len(), hash) {
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

/// Bytes whose LFs are found before their lines are read: 4 KiB keeps the LF list in a few cache lines.
const CHUNK: usize = 4096;

/// The lines starting in data[start..end], as one record vector per hour met. A record is the latency in the low
/// 32 bits and, above them, the endpoint's global id | 5xx << 11, so the hour's counts come from its records. Each
/// chunk's LFs are found first, so a line is read from both its ends and no line waits for the one before it to be
/// parsed. Reads may pass the input's end by up to 128 bytes: main maps zeros there.
fn parse(
    data: &[u8],
    start: usize,
    end: usize,
    table: &mut Table,
    dict: &Dict,
) -> Vec<(usize, Vec<u64>)> {
    let d = data.as_ptr();
    // Spec 2: every line is at least 40 bytes, so the block holds at most `most` lines, a chunk at most CHUNK / 40 + 1,
    // and a 64-byte window at most two LFs: both possible ones are stored and only the real ones counted.
    let most = (end - start) / 40 + 1;
    let mut ends = [0u32; CHUNK / 40 + 3];
    let mut runs: Vec<(usize, Vec<u64>)> = Vec::new();
    let (mut prev_prefix, mut at, mut p) = (u64::MAX, 0, start);
    // The current hour's next record. Each run holds a record for every line the block can have, so the writes need
    // no capacity check; a run's length is set when the hour changes.
    let mut w: *mut u64 = std::ptr::null_mut();
    let close = |run: &mut Vec<u64>, w: *mut u64| unsafe {
        run.set_len(w.offset_from(run.as_ptr()) as usize)
    };
    for from in (start..end).step_by(CHUNK) {
        let to = (from + CHUNK).min(end);
        let mut n = 0;
        for i in (from..to).step_by(64) {
            // Safety: the window is inside the input or its zero tail, and n + 2 <= ends.len().
            unsafe {
                let m = (mask32(d.add(i), b'\n') as u64
                    | (mask32(d.add(i + 32), b'\n') as u64) << 32)
                    & _bzhi_u64(!0, (to - i).min(64) as u32);
                *ends.get_unchecked_mut(n) = (i - from) as u32 + m.trailing_zeros();
                *ends.get_unchecked_mut(n + 1) =
                    (i - from) as u32 + (m & m.wrapping_sub(1)).trailing_zeros();
                n += m.count_ones() as usize;
            }
        }
        for &lf in &ends[..n] {
            let lf = from + lf as usize;
            // Lines usually share the previous line's hour, so compare it first: spec 2's year is always 2026, so bytes
            // 5..13 ("MM-DDTHH") name the hour, one u64 load (a-56).
            // Safety: a line has at least 40 bytes.
            let prefix = unsafe { (d.add(p + 5) as *const u64).read_unaligned() };
            if prefix != prev_prefix {
                if !w.is_null() {
                    close(&mut runs[at].1, w);
                }
                let h = hour_index(&data[p..]);
                at = match runs.iter().position(|r| r.0 == h) {
                    Some(i) => i,
                    None => {
                        runs.push((h, Vec::with_capacity(most)));
                        runs.len() - 1
                    }
                };
                let run = &mut runs[at].1;
                // Safety: the run's length is within its capacity.
                w = unsafe { run.as_mut_ptr().add(run.len()) };
                prev_prefix = prefix;
            }
            // Safety: the line and the 60 bytes after its start are inside the input or its zero tail.
            let (ep, len, error, latency) = unsafe { line(d, p, lf) };
            let e = table.endpoint(dict, data, ep, len);
            // Safety: see w above.
            unsafe {
                w.write(latency as u64 | (e as u64 | error << 11) << 32);
                w = w.add(1);
            }
            p = lf + 1;
        }
    }
    if !w.is_null() {
        close(&mut runs[at].1, w);
    }
    runs
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

/// Writes a space and v in decimal at w and returns the end. All ten digits are made at once and the leading zeros
/// shifted out, so every width runs the same instructions with no branch; the 16-byte store passes the number's end
/// by up to 6 bytes, which the next write covers. Safety: w..w + 17 must be writable.
unsafe fn put(w: *mut u8, v: u32) -> *mut u8 {
    let d = decimal8(v / 100_000_000) as u128 | (decimal8(v % 100_000_000) as u128) << 64;
    // The top bit keeps the last digit when v is 0.
    let lead = (d | 1 << 127).trailing_zeros() / 8;
    unsafe {
        *w = b' ';
        (w.add(1) as *mut u128)
            .write_unaligned((d | u128::from_ne_bytes([b'0'; 16])) >> (8 * lead));
        w.add(17 - lead as usize)
    }
}

/// A record whose latency is below this sorts as a 32-bit row, latency << 11 | id, in two counting passes of 11 and
/// 10 bits; the others (0.24 pct on seed 1) are sorted on their own and follow the rows, being longer than all of them.
const ROW_LATENCY: u32 = 1 << 21;
/// The most a line takes: label, an endpoint of up to 125 bytes (spec 2), six numbers of up to 10 digits, LF. An
/// hour's buffer holds every endpoint's line, plus the bytes a copy or a number store runs past the last one.
const LINE_MAX: usize = 21 + 125 + 6 * 11 + 1;
const TEXT_CAP: usize = IDS * LINE_MAX + 128;

/// One finishing thread's buffers, kept between hours: the packed rows and their second copy.
#[derive(Default)]
struct Space {
    rows: Vec<u32>,
    temp: Vec<u32>,
    long: Vec<u64>,
}

/// v[..n], grown with zeros first if it is shorter: the buffers keep their length between hours, so they are zeroed
/// only when an hour is the biggest so far.
fn prefix(v: &mut Vec<u32>, n: usize) -> &mut [u32] {
    if v.len() < n {
        v.resize(n, 0);
    }
    &mut v[..n]
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
    spaces: Vec<Space>,          // finishing buffers not in use
}

impl State {
    /// Hands block b's records to their hours; a record for an hour already claimed marks the run late.
    fn register(&mut self, b: usize, runs: Vec<(usize, Vec<u64>)>) {
        self.top
            .insert(b, runs.iter().map(|r| r.0).max().unwrap_or(0));
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
    free: Vec<Vec<u8>>, // written hours' buffers, for the next hours
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
        let n = (b * BLOCK).min(self.data.len());
        n.checked_sub(1)
            .map_or(0, |i| find(self.data, i, b'\n') + 1)
    }

    /// One of the run's `workers` threads; an hour is finished once the registered prefix holds a line `lag` hours later.
    fn work(&self, workers: usize, lag: usize) {
        let nblocks = self.data.len().div_ceil(BLOCK);
        let mut table = Table {
            slots: Box::new([0; SLOTS]),
            synced: 0,
        };
        let mut s = self.state.lock().unwrap();
        while !s.late {
            if s.next_hour == HOURS {
                drop(s);
                return self.release();
            } else if s.watermark == nblocks || s.next_hour + lag <= s.prefix_max {
                let h = s.next_hour;
                s.next_hour += 1;
                let runs = s.runs.remove(&h).unwrap_or_default();
                let mut space = s.spaces.pop().unwrap_or_default();
                drop(s);
                self.ready.notify_all();
                self.emit(h, self.finish(h, runs, &mut space));
                s = self.state.lock().unwrap();
                s.spaces.push(space);
                continue;
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

    /// Hour h's output lines in endpoint byte order (spec 4). The records are sorted by latency (two stable counting
    /// passes) and then dealt out by endpoint in byte order (a third), so each endpoint's latencies arrive sorted and a
    /// percentile is one read: no comparison sort, no select, no branch per record.
    fn finish(&self, h: usize, runs: Vec<Vec<u64>>, space: &mut Space) -> Vec<u8> {
        let order = self.dict.order();
        let mut count = [0u32; 2 * IDS]; // by id | 5xx << 11
        let mut digits = [[0u32; 2048]; 2]; // latency bits 0..11 and 11..21
        let total = runs.iter().map(Vec::len).sum();
        let Space { rows, temp, long } = space;
        let (rows, temp, mut k) = (
            prefix(rows, total).as_mut_ptr(),
            prefix(temp, total).as_mut_ptr(),
            0,
        );
        long.clear();
        for run in &runs {
            for &x in run {
                let (key, latency) = ((x >> 32) as usize & (2 * IDS - 1), x as u32);
                count[key] += 1;
                if latency < ROW_LATENCY {
                    digits[0][latency as usize & 2047] += 1;
                    digits[1][latency as usize >> 11 & 2047] += 1;
                    // Safety: k counts the rows so far, at most total.
                    unsafe { *rows.add(k) = latency << 11 | (key & (IDS - 1)) as u32 };
                    k += 1;
                } else {
                    long.push(x);
                }
            }
        }
        drop(runs); // the records are packed: they go back before the sort
        for d in &mut digits {
            d.iter_mut()
                .fold(0, |sum, c| sum + std::mem::replace(c, sum));
        }
        // Safety (both passes): a digit's slots run from the rows before it to the rows up to it, all below k.
        for (from, to, shift) in [(rows, temp, 11), (temp, rows, 22)] {
            let d = &mut digits[(shift == 22) as usize];
            for i in 0..k {
                unsafe {
                    let r = *from.add(i);
                    let at = &mut d[(r >> shift) as usize & 2047];
                    *to.add(*at as usize) = r;
                    *at += 1;
                }
            }
        }
        long.sort_unstable_by_key(|&x| x as u32);
        // The deal: each endpoint's span in byte order, filled in latency order.
        let (mut start, mut sum) = ([0u32; IDS], 0);
        for &id in &order {
            start[id as usize] = sum;
            sum += count[id as usize] + count[id as usize | IDS];
        }
        let mut next = start;
        // Safety: an endpoint's slots run from its start to its start plus its count, all below total.
        let mut deal = |id: usize, latency: u32| unsafe {
            *temp.add(next[id] as usize) = latency;
            next[id] += 1;
        };
        for i in 0..k {
            // Safety: i < k.
            deal(
                unsafe { *rows.add(i) } as usize & (IDS - 1),
                unsafe { *rows.add(i) } >> 11,
            );
        }
        long.iter()
            .for_each(|&x| deal((x >> 32) as usize & (IDS - 1), x as u32));
        let temp = &space.temp[..total];

        let mut label = [b' '; 32];
        label[..20].copy_from_slice(
            format!("{}T{:02}:00:00Z", DAYS[(h + 18) / 24], (h + 18) % 24).as_bytes(),
        );
        let mut buf = self
            .out
            .lock()
            .unwrap()
            .free
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(TEXT_CAP));
        let mut w = buf.as_mut_ptr();
        for &id in &order {
            let (errors, id) = (count[id as usize | IDS], id as usize);
            let n = count[id] + errors;
            if n == 0 {
                continue;
            }
            let sorted = &temp[start[id] as usize..][..n as usize];
            // Spec 3.4: 0-based index of rank ceil(p * n / 100).
            let at = |p: usize| sorted[(p * n as usize + 99) / 100 - 1];
            // Spec 3.6: error_ppm = floor(errors * 1000000 / count), in 64 bits.
            let ppm = (errors as u64 * 1_000_000 / n as u64) as u32;
            let name = self.dict.name(id);
            // Safety: buf holds TEXT_CAP bytes, enough for every endpoint's longest line and the bytes the label, name
            // and number stores run past it; each copy reads only its own source's bytes (label 32, name 128).
            unsafe {
                std::ptr::copy_nonoverlapping(label.as_ptr(), w, 32);
                std::ptr::copy_nonoverlapping(name.0.as_ptr(), w.add(21), 128);
                w = w.add(21 + name.0[127] as usize);
                for v in [n, at(50), at(95), at(99), errors, ppm] {
                    w = put(w, v);
                }
                *w = b'\n';
                w = w.add(1);
            }
        }
        // Safety: every byte below w is written above.
        unsafe { buf.set_len(w.offset_from(buf.as_ptr()) as usize) };
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
            let Some(mut buf) = o.parts.remove(&w) else {
                break;
            };
            drop(o);
            let mut file = self.file;
            file.write_all(&buf).expect("write output");
            buf.clear();
            o = self.out.lock().unwrap();
            o.free.push(buf);
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
    // Map the file read-only over an anonymous reservation 4 KiB longer (PROT_READ 1; MAP_PRIVATE | MAP_ANONYMOUS
    // 0x22, then MAP_PRIVATE | MAP_FIXED 0x12), never unmapped, so the slice lives for the run and the bytes past the
    // input read as zeros: the kernel zero-fills the file's last page and the reservation is zeros. Then ask the
    // kernel to read the file ahead (MADV_WILLNEED 3), a hint whose result is ignored.
    let room = unsafe { mmap(std::ptr::null_mut(), len + 4096, 1, 0x22, -1, 0) };
    let p = unsafe { mmap(room, len, 1, 0x12, file.as_raw_fd(), 0) };
    assert!(room as isize != -1 && p == room, "mmap failed");
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
