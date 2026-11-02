//! Track A: per (hour, endpoint) request count, nearest-rank p50/p95/p99 latency and 5xx rate.
//! The threads claim an mmap of /data/requests.log in 256 KiB blocks, in file order, and write each line as a 32-bit
//! row into its hour's region. An hour is finished (counted, selected, formatted) as soon as the registered prefix of
//! the file holds a line two hours later, while other threads keep parsing; its text is written over its rows, placed
//! in hour order and written at its offset, and the region given back: only a few hours' rows are alive at once.
//! Main tricks: parse and line (a-56, a-57), Table (a-10, a-40, a-57), finish and select_ranks (a-58), Rows (a-59), the
//! pipeline (a-55).

use std::arch::x86_64::*;
use std::sync::atomic::{AtomicUsize, Ordering::Acquire, Ordering::Relaxed, Ordering::Release};
use std::sync::{Condvar, Mutex};
use std::{
    cell::UnsafeCell, collections::BTreeMap, fs::File, os::fd::AsRawFd, os::unix::fs::FileExt,
};

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
    fn prctl(option: i32, ...) -> i32;
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

/// Writes a row for every line starting in data[start..end] into its hour's region (see `Rows`), through cur, this
/// thread's open chunk per hour, and returns the lowest and highest hour met. Each chunk's LFs are found first, so a
/// line is read from both its ends and no line waits for the one before it to be parsed. Reads may pass the input's
/// end by up to 128 bytes: main maps zeros there.
fn parse(
    data: &[u8],
    (start, end): (usize, usize),
    table: &mut Table,
    dict: &Dict,
    rows: &Rows,
    cur: &mut [(*mut u32, *mut u32); HOURS],
) -> (usize, usize) {
    let d = data.as_ptr();
    // Spec 2: every line is at least 40 bytes, so a chunk holds at most CHUNK / 40 + 1 lines, and a 64-byte window at
    // most two LFs: both possible ones are stored and only the real ones counted.
    let mut ends = [0u32; CHUNK / 40 + 3];
    let (mut prev_prefix, mut p, mut h, mut low, mut high) = (u64::MAX, start, 0, HOURS, 0);
    let mut c = cur[0]; // hour h's chunk: next row, last row but one
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
                cur[h] = c;
                h = hour_index(&data[p..]);
                (c, low, high, prev_prefix) = (cur[h], low.min(h), high.max(h), prefix);
            }
            // Safety: the line and the 60 bytes after its start are inside the input or its zero tail.
            let (ep, len, error, latency) = unsafe { line(d, p, lf) };
            let e = table.endpoint(dict, data, ep, len);
            let row = (e + 1) << 21 | (error as u32) << 20;
            if latency < ESCAPE {
                if c.0 == c.1 {
                    c = rows.claim(h);
                }
                // Safety: c.0 < c.1, the claimed chunk's end.
                unsafe {
                    *c.0 = row | latency;
                    c.0 = c.0.add(1);
                }
            } else {
                rows.long(h, row, latency);
            }
            p = lf + 1;
        }
    }
    cur[h] = c;
    (low, high)
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
    unsafe {
        *w = b' ';
        if v < 100_000_000 {
            // The top bit keeps the last digit when v is 0.
            let lead = (decimal8(v) | 1 << 63).trailing_zeros() / 8;
            (w.add(1) as *mut u64)
                .write_unaligned((decimal8(v) | 0x3030_3030_3030_3030) >> (8 * lead));
            return w.add(9 - lead as usize);
        }
        let d = decimal8(v / 100_000_000) as u128 | (decimal8(v % 100_000_000) as u128) << 64;
        let lead = (d | 1 << 127).trailing_zeros() / 8;
        (w.add(1) as *mut u128)
            .write_unaligned((d | u128::from_ne_bytes([b'0'; 16])) >> (8 * lead));
        w.add(17 - lead as usize)
    }
}

/// A row is (endpoint id + 1) << 21 | 5xx << 20 | latency, 0 for a slot no line took; a line with a latency of ESCAPE
/// or more (0.24 pct on seed 1) goes to its hour's long area instead, as the row without the latency, then the latency.
const ESCAPE: u32 = (1 << 20) - 1;
/// Rows a thread claims in one hour's region at a time.
const CLAIM: usize = 256;
/// An hour's text is written over its rows, so the latencies the finish copies out start at least this many rows
/// (512 KiB) in: past the longest text (spec 2: 1,842 endpoints of at most 213 bytes a line).
const SPARE: usize = 1 << 17;

/// Every hour's rows: one region each in an anonymous reservation (MAP_NORESERVE), of which only the pages touched
/// cost memory. Threads claim CLAIM rows at a time, so a row needs no atomic; an hour's region holds its rows, then
/// its latencies grouped by endpoint, then at long_at its long area, and is given back once its text is written.
struct Rows {
    base: *mut u32,
    stride: usize,
    long_at: usize,
    claimed: [AtomicUsize; HOURS],
    longs: [AtomicUsize; HOURS],
}

// Safety: threads write only the chunks they claimed; an hour's region is read only by the thread finishing it.
unsafe impl Sync for Rows {}

impl Rows {
    fn new(len: usize, workers: usize) -> Rows {
        // No transparent huge pages (PR_SET_THP_DISABLE 41): with THP on, an hour's first row would charge 2 MiB.
        unsafe { prctl(41, 1usize, 0usize, 0usize, 0usize) };
        // A row per line (spec 2: a line has at least 40 bytes) and one partly used chunk per thread, the latencies
        // and a trash slot, then two slots per line for the long area; 1,024 rows are one 4 KiB page, so regions
        // start on pages.
        let lines = len / 40 + 1;
        let stride = (SPARE + 4 * lines + workers * CLAIM + 1).next_multiple_of(1024);
        // PROT_READ | PROT_WRITE 3; MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE 0x4022.
        let base = unsafe { mmap(std::ptr::null_mut(), stride * HOURS * 4, 3, 0x4022, -1, 0) };
        assert!(base as isize != -1, "mmap failed");
        Rows {
            base: base as *mut u32,
            stride,
            long_at: stride - 2 * lines,
            claimed: [const { AtomicUsize::new(0) }; HOURS],
            longs: [const { AtomicUsize::new(0) }; HOURS],
        }
    }

    /// Hour h's region.
    fn region(&self, h: usize) -> *mut u32 {
        self.base.wrapping_add(h * self.stride)
    }

    /// A new chunk of hour h: its first row and its end.
    fn claim(&self, h: usize) -> (*mut u32, *mut u32) {
        let at = self
            .region(h)
            .wrapping_add(self.claimed[h].fetch_add(CLAIM, Relaxed));
        (at, at.wrapping_add(CLAIM))
    }

    /// A line of hour h with a latency of ESCAPE or more: its row without the latency, and the latency.
    #[cold]
    fn long(&self, h: usize, row: u32, latency: u32) {
        let at = self.longs[h].fetch_add(2, Relaxed);
        // Safety: the long area holds two slots for every line the input can have.
        unsafe {
            let p = self.region(h).add(self.long_at + at);
            (*p, *p.add(1)) = (row, latency);
        }
    }
}

/// Row m: the lanes whose bit is set in m, in order, then the clear lanes, in order; one byte per lane.
const fn perms() -> [u64; 256] {
    let (mut t, mut m) = ([0u64; 256], 0);
    while m < 256 {
        let (mut j, mut n) = (0, 0);
        while j < 16 {
            if (m >> (j & 7) & 1 == 1) == (j < 8) {
                t[m] |= ((j & 7) as u64) << (8 * n);
                n += 1;
            }
            j += 1;
        }
        m += 1;
    }
    t
}
static PERM: [u64; 256] = perms();

/// out[i] = the value at 0-based position ranks[i] of a once sorted ascending. ranks is ascending (repeats allowed)
/// and each rank is below a.len(); a is reordered freely; b is scratch of at least a.len().
fn select_ranks(a: &mut [i32], b: &mut [i32], ranks: &[u32], out: &mut [i32]) {
    debug_assert!(b.len() >= a.len() && ranks.iter().all(|&r| (r as usize) < a.len()));
    // Safety: go reads and writes only a[..len] and b[..len].
    unsafe { go(a.as_mut_ptr(), b.as_mut_ptr(), 0, a.len(), ranks, out) }
}

/// Quickselect on src[lo..hi), partitioning into dst; the two swap roles at each level.
unsafe fn go(src: *mut i32, dst: *mut i32, lo: usize, hi: usize, ranks: &[u32], out: &mut [i32]) {
    if ranks.is_empty() {
        return;
    }
    let n = hi - lo;
    unsafe {
        if n <= 16 {
            let mut x = [0i32; 16];
            let (s0, s1) = small(src.add(lo), n);
            _mm256_storeu_si256(x.as_mut_ptr() as *mut __m256i, s0);
            _mm256_storeu_si256(x.as_mut_ptr().add(8) as *mut __m256i, s1);
            for (o, &r) in out.iter_mut().zip(ranks) {
                *o = x[r as usize - lo];
            }
            return;
        }
        let pivot = median3(*src.add(lo), *src.add(lo + n / 2), *src.add(hi - 1));
        let cut = partition(src, dst, lo, hi, pivot);
        if cut == lo {
            // The pivot is the range's minimum: split off every value equal to it.
            if pivot == i32::MAX {
                out.fill(i32::MAX);
                return;
            }
            let cut = partition(src, dst, lo, hi, pivot + 1);
            let k = ranks.partition_point(|&r| (r as usize) < cut);
            out[..k].fill(pivot);
            return go(dst, src, cut, hi, &ranks[k..], &mut out[k..]);
        }
        let k = ranks.partition_point(|&r| (r as usize) < cut);
        let (below, above) = out.split_at_mut(k);
        go(dst, src, lo, cut, &ranks[..k], below);
        go(dst, src, cut, hi, &ranks[k..], above);
    }
}

fn median3(a: i32, b: i32, c: i32) -> i32 {
    a.max(b).min(c.max(a.min(b)))
}

/// Partitions src[lo..hi) by pivot into dst[lo..hi): values below it from lo up, the rest from hi down; returns the cut.
unsafe fn partition(src: *const i32, dst: *mut i32, lo: usize, hi: usize, pivot: i32) -> usize {
    let (mut l, mut r, mut t) = (lo, hi, lo);
    unsafe {
        let pv = _mm256_set1_epi32(pivot);
        // r - l == hi - t throughout, so with 16 or more values unread both 8-lane stores land inside dst[l..r) apart.
        while hi - t >= 16 {
            let v = _mm256_loadu_si256(src.add(t) as *const __m256i);
            let m = _mm256_movemask_ps(_mm256_castsi256_ps(_mm256_cmpgt_epi32(pv, v))) as usize;
            let p = _mm256_permutevar8x32_epi32(
                v,
                _mm256_cvtepu8_epi32(_mm_cvtsi64_si128(PERM[m] as i64)),
            );
            let c = m.count_ones() as usize;
            _mm256_storeu_si256(dst.add(l) as *mut __m256i, p);
            _mm256_storeu_si256(dst.add(r - 8) as *mut __m256i, p);
            l += c;
            r -= 8 - c;
            t += 8;
        }
        while t < hi {
            let v = *src.add(t);
            *dst.add(l) = v;
            *dst.add(r - 1) = v;
            l += (v < pivot) as usize;
            r -= (v >= pivot) as usize;
            t += 1;
        }
    }
    l
}

/// v and its lanes permuted by p, compared: the lanes set in M take the larger value.
#[inline(always)]
unsafe fn cx<const M: i32>(v: __m256i, p: __m256i) -> __m256i {
    unsafe {
        let w = _mm256_permutevar8x32_epi32(v, p);
        _mm256_blend_epi32::<M>(_mm256_min_epi32(v, w), _mm256_max_epi32(v, w))
    }
}

#[inline(always)]
unsafe fn idx(a: [i32; 8]) -> __m256i {
    unsafe { _mm256_loadu_si256(a.as_ptr() as *const __m256i) }
}

/// A bitonic register sorted ascending.
#[inline(always)]
unsafe fn clean8(v: __m256i) -> __m256i {
    unsafe {
        let v = cx::<0xF0>(v, idx([4, 5, 6, 7, 0, 1, 2, 3]));
        let v = cx::<0xCC>(v, idx([2, 3, 0, 1, 6, 7, 4, 5]));
        cx::<0xAA>(v, idx([1, 0, 3, 2, 5, 4, 7, 6]))
    }
}

/// One register sorted ascending.
#[inline(always)]
unsafe fn sort8(v: __m256i) -> __m256i {
    unsafe {
        let p1 = idx([1, 0, 3, 2, 5, 4, 7, 6]);
        let v = cx::<0xAA>(v, p1);
        let v = cx::<0xCC>(v, idx([3, 2, 1, 0, 7, 6, 5, 4]));
        let v = cx::<0xAA>(v, p1);
        clean8(cx::<0xF0>(v, idx([7, 6, 5, 4, 3, 2, 1, 0])))
    }
}

/// The n <= 16 values at p, padded with i32::MAX and sorted ascending across two registers.
#[inline(always)]
unsafe fn small(p: *const i32, n: usize) -> (__m256i, __m256i) {
    unsafe {
        let (max, n) = (_mm256_set1_epi32(i32::MAX), _mm256_set1_epi32(n as i32));
        let m0 = _mm256_cmpgt_epi32(n, idx([0, 1, 2, 3, 4, 5, 6, 7]));
        let v0 = sort8(_mm256_blendv_epi8(max, _mm256_maskload_epi32(p, m0), m0));
        if _mm256_testz_si256(_mm256_cmpgt_epi32(n, _mm256_set1_epi32(8)), max) != 0 {
            return (v0, max);
        }
        let m1 = _mm256_cmpgt_epi32(n, idx([8, 9, 10, 11, 12, 13, 14, 15]));
        let v1 = sort8(_mm256_blendv_epi8(
            max,
            _mm256_maskload_epi32(p.add(8), m1),
            m1,
        ));
        let w = _mm256_permutevar8x32_epi32(v1, idx([7, 6, 5, 4, 3, 2, 1, 0]));
        (
            clean8(_mm256_min_epi32(v0, w)),
            clean8(_mm256_max_epi32(v0, w)),
        )
    }
}

/// The run's shared progress, under one lock.
#[derive(Default)]
struct State {
    next_block: usize,
    top: BTreeMap<usize, usize>, // per registered block past the watermark: its last hour
    watermark: usize,            // every block below it is registered
    prefix_max: usize,           // the last hour in the blocks below the watermark
    next_hour: usize,            // every hour below it is claimed for finishing
    late: bool,                  // a block brought lines of an hour already claimed
}

impl State {
    /// Records that block b, with lines of hours low to high, is parsed; a line of an hour already claimed marks the run
    /// late.
    fn register(&mut self, b: usize, (low, high): (usize, usize)) {
        self.late |= low < self.next_hour;
        self.top.insert(b, high);
        while let Some(t) = self.top.remove(&self.watermark) {
            self.prefix_max = self.prefix_max.max(t);
            self.watermark += 1;
        }
    }
}

/// Finished hours waiting for the ones before them, how many are placed, and where the next one goes.
#[derive(Default)]
struct Out {
    parts: BTreeMap<usize, &'static [u8]>,
    placed: usize,
    offset: u64,
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
    rows: Rows,
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
        let (mut cur, mut scratch) = (
            [(std::ptr::null_mut(), std::ptr::null_mut()); HOURS],
            Vec::new(),
        );
        let mut s = self.state.lock().unwrap();
        while !s.late {
            if s.next_hour == HOURS {
                drop(s);
                return self.release();
            } else if s.watermark == nblocks || s.next_hour + lag <= s.prefix_max {
                let h = s.next_hour;
                s.next_hour += 1;
                drop(s);
                self.ready.notify_all();
                self.emit(h, self.finish(h, &mut scratch));
            } else if s.next_block < nblocks && s.next_block < s.watermark + 2 * workers {
                let b = s.next_block;
                s.next_block += 1;
                drop(s);
                let span = (self.align(b), self.align(b + 1));
                let hours = parse(self.data, span, &mut table, self.dict, &self.rows, &mut cur);
                self.state.lock().unwrap().register(b, hours);
                self.ready.notify_all();
            } else {
                s = self.ready.wait(s).unwrap();
                continue;
            }
            s = self.state.lock().unwrap();
        }
    }

    /// Hour h's output lines in endpoint byte order (spec 4). The rows are counted per endpoint and their latencies
    /// copied to each endpoint's span in byte order; p50, p95 and p99 come from one quickselect per span for all three
    /// ranks: two passes over the rows and no sort. The text is written over the rows, in the hour's region.
    fn finish(&self, h: usize, scratch: &mut Vec<i32>) -> &'static [u8] {
        let order = self.dict.order();
        let (claimed, longs) = (
            self.rows.claimed[h].load(Relaxed),
            self.rows.longs[h].load(Relaxed),
        );
        // Safety: the region is stride rows; its rows are all written (their blocks are registered) and no other thread
        // touches it until it is given back.
        let region =
            unsafe { std::slice::from_raw_parts_mut(self.rows.region(h), self.rows.stride) };
        let (region, long) = region.split_at_mut(self.rows.long_at);
        let (head, values) = region.split_at_mut(claimed.max(SPARE));
        // Safety: u32 and i32 share size and alignment; the select reads the spans as i32.
        let values = unsafe {
            std::slice::from_raw_parts_mut(values.as_mut_ptr() as *mut i32, values.len())
        };
        let (rows, long) = (&head[..claimed], &long[..longs]);
        let mut count = [0u32; 2 * IDS]; // by (id + 1) << 1 | 5xx; a free slot counts at 0
        for &r in rows.iter().chain(long.iter().step_by(2)) {
            count[(r >> 20) as usize] += 1;
        }
        // By id + 1; start[0] is a trash slot past every span, where the free slots go.
        let (mut start, mut sum, mut big) = ([0u32; IDS], 0, 0);
        for &id in &order {
            let k = id as usize + 1;
            let n = count[2 * k] + count[2 * k + 1];
            (start[k], sum, big) = (sum, sum + n, big.max(n));
        }
        start[0] = sum;
        let mut next = start;
        for &r in rows {
            let k = (r >> 21) as usize;
            // Safety: an endpoint's slots run from its start to its start plus its count, all below sum, and the trash
            // slot is sum, all inside values (a row per line past head). The top bit is flipped so the select's i32
            // order is the latencies' u32 order.
            unsafe { *values.get_unchecked_mut(next[k] as usize) = (r & ESCAPE ^ 1 << 31) as i32 };
            next[k] += (k != 0) as u32;
        }
        for pair in long.chunks_exact(2) {
            let k = (pair[0] >> 21) as usize;
            values[next[k] as usize] = (pair[1] ^ 1 << 31) as i32;
            next[k] += 1;
        }
        if scratch.len() < big as usize {
            scratch.resize(big as usize, 0);
        }

        let mut label = [b' '; 32];
        label[..20].copy_from_slice(
            format!("{}T{:02}:00:00Z", DAYS[(h + 18) / 24], (h + 18) % 24).as_bytes(),
        );
        let text = head.as_mut_ptr() as *mut u8;
        let mut w = text;
        for &id in &order {
            let (id, k) = (id as usize, id as usize + 1);
            let (n, errors) = (count[2 * k] + count[2 * k + 1], count[2 * k + 1]);
            if n == 0 {
                continue;
            }
            // Spec 3.4: 0-based index of rank ceil(p * n / 100).
            let rank = |p: usize| ((p * n as usize + 99) / 100 - 1) as u32;
            let mut at = [0i32; 3];
            select_ranks(
                &mut values[start[k] as usize..][..n as usize],
                scratch,
                &[rank(50), rank(95), rank(99)],
                &mut at,
            );
            let [p50, p95, p99] = at.map(|v| v as u32 ^ 1 << 31);
            // Spec 3.6: error_ppm = floor(errors * 1000000 / count), in 64 bits.
            let ppm = (errors as u64 * 1_000_000 / n as u64) as u32;
            let name = self.dict.name(id);
            // Safety: head holds at least SPARE rows (512 KiB), past every endpoint's longest line and the bytes the
            // label, name and number stores run over it; each copy reads only its own source's bytes (label 32, name
            // 128).
            unsafe {
                std::ptr::copy_nonoverlapping(label.as_ptr(), w, 32);
                std::ptr::copy_nonoverlapping(name.0.as_ptr(), w.add(21), 128);
                w = w.add(21 + name.0[127] as usize);
                for v in [n, p50, p95, p99, errors, ppm] {
                    w = put(w, v);
                }
                *w = b'\n';
                w = w.add(1);
            }
        }
        // Safety: every byte from text to w is written above, and the region is never unmapped.
        unsafe { std::slice::from_raw_parts(text, w.offset_from(text) as usize) }
    }

    /// Hands over hour h's text, places every hour that is ready in order, and writes those at their offsets, so
    /// threads write at once; each region is given back once its text is out.
    fn emit(&self, h: usize, text: &'static [u8]) {
        let mut ready = Vec::new();
        {
            let mut guard = self.out.lock().unwrap();
            let o = &mut *guard;
            o.parts.insert(h, text);
            while let Some(t) = o.parts.remove(&o.placed) {
                ready.push((o.placed, o.offset, t));
                (o.placed, o.offset) = (o.placed + 1, o.offset + t.len() as u64);
            }
        }
        for (h, offset, t) in ready {
            self.file.write_all_at(t, offset).expect("write output");
            // Safety: the region is the hour's, page-aligned, and nothing reads it any more; 4 is MADV_DONTNEED.
            unsafe { madvise(self.rows.region(h) as *const u8, self.rows.stride * 4, 4) };
        }
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
        rows: Rows::new(data.len(), workers),
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
        run(data, &dict, &out, workers, HOURS);
    }
    // Everything is written; exit without freeing what the kernel reclaims anyway.
    std::process::exit(0);
}
