//! Track A: per (hour, endpoint) request count, nearest-rank p50/p95/p99 latency and 5xx rate.
//! One pass over an mmap of /data/requests.log, split into byte ranges, one private table per thread;
//! the merge, percentile selection and formatting then run in parallel, one hour per task.
//! Main tricks: line_fields (a-5), parse_latency (a-13), Table (a-10, a-17), Hour (a-29), percentiles (a-20), release_range (a-12).

use std::sync::{Mutex, atomic::AtomicUsize, atomic::Ordering::Relaxed, mpsc};
use std::{fs::File, io::Write, os::fd::AsRawFd, thread};

// Spec 2 grammar: ts (27 bytes), SP, method (3 to 7 bytes), SP: the endpoint starts at byte 32 to 36 of a line.
const ENDPOINT_MIN: usize = 32;
// Spec 2 grammar: ts = "YYYY-MM-DDTHH..."; bytes 0..13 name the UTC hour.
const HOUR_PREFIX: usize = 13;

unsafe extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn madvise(addr: *const u8, len: usize, advice: i32) -> i32;
    fn mallopt(param: i32, value: i32) -> i32;
}

/// Drops the page-table entries of data[start..end] once its scan is done, so the exit has fewer to tear down.
/// The mapping is private and never written: a later read of these bytes (an endpoint the tail prints)
/// faults the same bytes back in from the page cache. Only whole pages inside the range are released.
/// Measured on this VM (seed 1 full, 20 runs; a-12): output closed -> process reaped 1.9-2.0 -> 0.8-1.3 ms at 1, 4
/// and 12 workers; the work before it unchanged within noise.
fn release_range(data: &[u8], start: usize, end: usize) {
    let (lo, hi) = (start.div_ceil(4096) * 4096, end / 4096 * 4096);
    // Safety: [lo, hi) lies inside the mapping, which nothing writes. 4 is MADV_DONTNEED. A range
    // shorter than one page gives length 0, a no-op; wrapping_add covers lo past the input's end.
    unsafe { madvise(data.as_ptr().wrapping_add(lo), hi.saturating_sub(lo), 4) };
}

/// The first `byte` at or after `from`, or the input's end.
fn find(data: &[u8], from: usize, byte: u8) -> usize {
    let pos = data[from..].iter().position(|&b| b == byte);
    pos.map_or(data.len(), |off| from + off)
}

/// Splits data into `parts` ranges whose starts are line starts; a line belongs to the range holding its first byte.
fn line_ranges(data: &[u8], parts: usize) -> Vec<(usize, usize)> {
    // Range i starts at the first line start at or after len / parts * i: 0 (i = 0 or a very short
    // input) or a position right after an LF; i = parts is the input's end.
    let start = |i: usize| match data.len() / parts * i {
        _ if i == parts => data.len(),
        n => (find(data, n.saturating_sub(1), b'\n') + 1).min(data.len()) * (n != 0) as usize,
    };
    (0..parts).map(|i| (start(i), start(i + 1))).collect()
}

/// "YYYY-MM-DDTHH" as the decimal number YYYYMMDDHH: fits u32 and sorts like the text.
fn parse_hour(p: &[u8]) -> u32 {
    let n = |r: std::ops::Range<usize>| r.fold(0, |acc, i| acc * 10 + (p[i] - b'0') as u32);
    n(0..4) * 1_000_000 + n(5..7) * 10_000 + n(8..10) * 100 + n(11..13)
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

use std::arch::x86_64::*;

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

/// A record is one u32 per input line: the thread-local endpoint id in the low ID_BITS bits, the latency above
/// them. Spec 2 fixes 1,842 endpoints per seed, so ids stay below ID_ESC; a line whose id is ID_ESC or more, or whose
/// latency is LAT_ESC (2^21 us) or more, takes three words instead: ID_ESC, the id, the latency, so every input
/// stays exact. These records (a-29) replace a 16-byte group per (thread, hour, endpoint) whose latencies sat in
/// 3-byte slots in chunk chains (a-4, a-11): the scan keeps no per-group state.
const ID_BITS: u32 = 11;
const ID_ESC: u32 = (1 << ID_BITS) - 1;
const LAT_ESC: u32 = 1 << (32 - ID_BITS);

/// One thread's lines of one hour: request and 5xx counts per thread-local endpoint id, and the records in input
/// order. Kept apart per hour, so the tail takes each hour whole, as that hour's job, and drops it once placed.
#[derive(Default, Clone)]
struct Hour {
    hour: u32,
    counts: Vec<u32>,
    errors: Vec<u32>,
    recs: Vec<u32>,
}

/// Two-level key. An open-addressing (linear probing) table interns each endpoint to a local id; it holds
/// only the distinct endpoints, so it stays small and cache-resident. Each line then bumps its hour's two counters
/// at that id and appends one record, with no second probe.
/// Measured against one (hour, endpoint) table (a-6), this VM, seed 1 full: scan 90 -> 70 ns per line at
/// 1 worker, 94 -> 72 at 4; peak RssAnon -1.4 MB (1 worker) to -3 MB (12 workers).
/// After the scan the same table is what the thread hands to the tail.
#[derive(Default)]
struct Table {
    slots: Vec<u32>, // length << 24 | endpoint id + 1; 0 marks an empty slot
    keys: Vec<[__m256i; 2]>, // endpoint id -> its first two chunks (the second zero up to 32 bytes)
    eps: Vec<(u32, u8)>,    // endpoint id -> (offset in `pool`, length); spec 2: at most 125 bytes
    // Each distinct endpoint's bytes, copied here when first seen. Everything after the scan reads endpoints from
    // this pool, never from the mapping, whose page-table entries are released once its range is scanned: read
    // from the mapping, the global merge and the output refault those pages. Measured on this VM with the
    // per-endpoint merge below: merge 0.69 -> 0.27 ms at 4 workers, 2.27 -> 1.62 ms at 12 (a-17).
    // Each endpoint is zero-padded to a multiple of 32 bytes here, so a hit is confirmed chunk by chunk (a-40b).
    pool: Vec<u8>,
    hours: Vec<Hour>,
}

impl Table {
    fn hour(&mut self, hour: u32) -> usize {
        if let Some(i) = self.hours.iter().position(|h| h.hour == hour) {
            return i;
        }
        // a-47: reserved up front. recs at 1 << 16 entries (256 KB) is above glibc's mmap threshold (128 KiB, pinned
        // in main), so it is mapped on its own and only the pages written are charged, instead of doubling inside the
        // thread's heap and leaving each old block charged there; counts and errors start at the 2,048 ids that
        // ID_BITS allows, so add's resize is only a fallback. Measured effect: PERF.md a-47.
        self.hours.push(Hour { hour, counts: vec![0; 1 << ID_BITS], errors: vec![0; 1 << ID_BITS], recs: Vec::with_capacity(1 << 16) });
        self.hours.len() - 1
    }

    fn ep(&self, id: u32) -> &[u8] {
        &self.pool[self.eps[id as usize].0 as usize..][..self.eps[id as usize].1 as usize]
    }

    /// The local id of the endpoint b[s..s + len], interning it on first sight; the table doubles once it is half
    /// full. Its first chunk is the hash's input; a hit needs the slot's length and the stored first two chunks to
    /// match, and an endpoint over 64 bytes also compares its other chunks with its zero-padded pool copy, so there is
    /// no byte compare call and no length branch up to 64 bytes (94.5 % of seed 1's lines). The caller keeps
    /// s + 128 within b. a-40b, cachegrind, 1 worker, against a-40a's four-multiply hash and bcmp confirm: scan
    /// 218.2 -> 189.1 instructions per line (seed 1), 220.5 -> 190.7 (seed 4242); conditional mispredicts -0.71 per
    /// line (seed 1), -0.68 (seed 4242).
    #[inline(always)]
    fn endpoint(&mut self, b: &[u8], s: usize, len: usize) -> u32 {
        let v = Self::key(b, s, len);
        let mask = self.slots.len() - 1;
        let mut i = key_hash(v[0]) as usize & mask;
        // The probe loop calls nothing, so the key vectors stay in registers; a miss inserts after it.
        while self.slots[i] != 0 {
            let id = (self.slots[i] & 0xFF_FFFF) as usize - 1;
            // Safety: a nonzero slot holds an interned id + 1; keys and eps have one entry per interned id, and the
            // pool holds each endpoint's chunks whole. s + 128 <= b.len() (endpoint's caller), and endpoints have
            // at most 125 bytes (spec 2), so every chunk read is readable.
            // The length is compared first: equal lengths keep every chunk read inside the candidate's pool copy.
            let hit = self.slots[i] >> 24 == len as u32 && unsafe {
                let key = self.keys.get_unchecked(id);
                let p = b.as_ptr().add(s);
                let off = self.eps.get_unchecked(id).0 as usize;
                let q = self.pool.as_ptr().add(off);
                same(v[0], key[0])
                    & same(v[1], key[1])
                    & (len <= 64 || same(chunk(p.add(64), (len - 64).min(32)), chunk(q.add(64), 32)))
                    & (len <= 96 || same(chunk(p.add(96), (len - 96).min(32)), chunk(q.add(96), 32)))
            };
            if hit {
                return id as u32;
            }
            i = (i + 1) & mask;
        }
        self.insert(i, b, s, len)
    }

    /// An endpoint's first two chunks (the second zero up to 32 bytes). The caller keeps s + 128 within b.
    fn key(b: &[u8], s: usize, len: usize) -> [__m256i; 2] {
        // Safety: s + 128 <= b.len(), and an endpoint has at most 125 bytes (spec 2), so both chunks are readable.
        let (p, hi) = (b.as_ptr().wrapping_add(s), len.clamp(32, 64) - 32);
        unsafe { [chunk(p, len.min(32)), chunk(p.add(32), hi)] }
    }

    #[cold]
    fn insert(&mut self, i: usize, b: &[u8], s: usize, len: usize) -> u32 {
        self.eps.push((self.pool.len() as u32, len as u8));
        self.pool.extend_from_slice(&b[s..s + len]);
        self.pool.resize(self.pool.len().next_multiple_of(32), 0);
        self.keys.push(Self::key(b, s, len));
        self.slots[i] = self.eps.len() as u32 | (len as u32) << 24;
        if self.eps.len() * 2 > self.slots.len() {
            self.grow();
        }
        self.eps.len() as u32 - 1
    }

    fn grow(&mut self) {
        let n = self.slots.len() * 2;
        let old = std::mem::replace(&mut self.slots, vec![0; n]);
        for id in old.into_iter().filter(|&id| id != 0) {
            let mut i = key_hash(self.keys[(id & 0xFF_FFFF) as usize - 1][0]) as usize & (self.slots.len() - 1);
            while self.slots[i] != 0 {
                i = (i + 1) & (self.slots.len() - 1);
            }
            self.slots[i] = id;
        }
    }

    fn add(&mut self, at: usize, e: u32, error: bool, latency: u32) {
        let h = &mut self.hours[at];
        if e as usize >= h.counts.len() {
            let n = (e as usize + 1).next_power_of_two();
            h.counts.resize(n, 0);
            h.errors.resize(n, 0);
        }
        h.counts[e as usize] += 1;
        h.errors[e as usize] += error as u32;
        if e < ID_ESC && latency < LAT_ESC {
            h.recs.push(e | latency << ID_BITS);
        } else {
            h.recs.extend_from_slice(&[ID_ESC, e, latency]);
        }
    }
}

/// Aggregates every line starting in data[start..end].
fn scan(data: &'static [u8], start: usize, end: usize) -> Table {
    let mut table = Table::default();
    table.slots = vec![0; 1024];
    let (mut prev_prefix, mut at, mut p) = ((u64::MAX, u64::MAX), 0, start);
    while p < end {
        // Lines usually share the previous line's hour, so compare its 13-byte prefix first, as two overlapping
        // u64 loads (bytes 0..8 and 5..13) rather than a slice compare.
        let word = |i: usize| u64::from_le_bytes(data[i..i + 8].try_into().unwrap());
        let prefix = (word(p), word(p + HOUR_PREFIX - 8));
        if prefix != prev_prefix {
            at = table.hour(parse_hour(&data[p..p + HOUR_PREFIX]));
            prev_prefix = prefix;
        }
        let (ep_start, lf) = line_fields(data, p);
        let (sp, latency) = parse_latency(data, lf);
        // Spec 2 grammar: status is three digits, 100 to 599, followed by SP; it is a 5xx error exactly when its
        // first digit is '5', so only that byte is read.
        let error = data[sp - 3] == b'5';
        let len = sp - 4 - ep_start;
        let e = match data.get(..ep_start + 128) {
            Some(b) => table.endpoint(b, ep_start, len),
            None => {
                // The input's last 128 bytes: the endpoint is copied into a zeroed buffer, so no load passes the end.
                let mut buf = [0u8; 128];
                buf[..len].copy_from_slice(&data[ep_start..sp - 4]);
                table.endpoint(&buf, 0, len)
            }
        };
        table.add(at, e, error, latency);
        p = lf + 1;
    }
    table
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

/// An endpoint's first 16 bytes as a big-endian integer, zero-padded: integer order equals byte order on
/// those bytes, and a shorter endpoint (spec 2: no zero bytes) sorts first, as spec 4 requires.
fn prefix_key(ep: &[u8]) -> u128 {
    u128::from_be_bytes(std::array::from_fn(|i| *ep.get(i).unwrap_or(&0)))
}

/// One global endpoint order: every thread's distinct endpoints merged by bytes (spec 4), giving the
/// sorted list of all endpoints and, per thread, local id -> global rank. Each thread's list is sorted in
/// parallel first. Each merge step takes the smallest 16-byte integer prefix among the threads' heads, the
/// smallest endpoint among the heads with that prefix (bytes compared only there), and advances every head
/// holding that endpoint at once: one step per distinct endpoint, not per (thread, endpoint).
/// It replaces the per-thread sort of every group by (hour, endpoint bytes) and the tail's byte sort.
/// Measured on this VM (seed 1 full, with the endpoint pool; a-17), the merge against one that picked one head per step
/// by comparing (prefix, bytes) across all heads: 1.38 -> 0.27 ms at 4 workers, 8.19 -> 1.62 ms at 12; the serial
/// window from the last scan end to the first tail job 2.56 -> 1.32 ms and 9.06 -> 2.31 ms.
/// It takes the sorted lists and drops them on return, so they are not held through the tail.
fn global_ranks(tables: &[Table], sorted: Vec<Vec<(u128, u32)>>) -> (Vec<&[u8]>, Vec<Vec<u32>>) {
    let mut ranks: Vec<Vec<u32>> = tables.iter().map(|t| vec![0; t.eps.len()]).collect();
    let (mut all, mut pos) = (Vec::new(), vec![0; tables.len()]);
    loop {
        // A thread's head as (prefix, bytes): tuple order compares the bytes only where the prefixes tie.
        let head = |t: usize| sorted[t].get(pos[t]).map(|&(k, e)| (k, tables[t].ep(e)));
        let Some(least) = (0..tables.len()).filter_map(head).min() else {
            return (all, ranks);
        };
        all.push(least.1);
        let at: Vec<usize> = (0..tables.len()).filter(|&t| head(t) == Some(least)).collect();
        for t in at {
            ranks[t][sorted[t][pos[t]].1 as usize] = all.len() as u32 - 1;
            pos[t] += 1;
        }
    }
}

fn main() {
    // Pin glibc's mmap threshold (M_MMAP_THRESHOLD, -3) at its 128 KiB default. Left dynamic, freeing the first
    // large record buffer raises it to 1 MiB, so the tail's output buffers come from the heap and their freed pages
    // stay charged while the output file grows. Measured (this VM, 12 workers, peak of RssAnon + output size; a-11):
    // 22.4 -> 19.8 MB median.
    unsafe { mallopt(-3, 128 * 1024) };
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
    let cpus = thread::available_parallelism().map_or(1, |n| n.get());
    let var = std::env::var("SD_WORKERS").unwrap_or_default();
    let workers = var.trim().parse().unwrap_or(cpus).max(1);
    let (mut tables, sorted): (Vec<Table>, Vec<_>) = thread::scope(|s| {
        let spawn = |&(a, b): &(usize, usize)| {
            s.spawn(move || {
                let mut t = scan(data, a, b);
                release_range(data, a, b);
                // The probe table and the endpoint keys are not needed after the scan; freed here, they are not held
                // through the tail. Freeing the keys (64 bytes per endpoint, a-40d), this VM, seed 1 full, 12 workers,
                // peak RssAnon + output, median of 11: 20.6 -> 19.4 MB (ce481c8: 19.1).
                (t.slots, t.keys) = (Vec::new(), Vec::new());
                // This thread's endpoint ids in byte order, sorted here in parallel for the global merge.
                let ids = 0..t.eps.len() as u32;
                let mut sorted: Vec<_> = ids.map(|e| (prefix_key(t.ep(e)), e)).collect();
                sorted.sort_unstable_by_key(|&(k, e)| (k, t.ep(e)));
                (t, sorted)
            })
        };
        let handles: Vec<_> = line_ranges(data, workers).iter().map(spawn).collect();
        handles.into_iter().map(|h| h.join().unwrap()).unzip()
    });

    // The tail is split by hour: hours are disjoint parts of the key space that are also contiguous in the
    // output, so each part is merged, selected and formatted by one thread with no shared writes, and the
    // parts are written in order. The hours come from the input itself.
    let mut hours: Vec<u32> = tables.iter().flat_map(|t| &t.hours).map(|h| h.hour).collect();
    hours.sort_unstable();
    hours.dedup();
    // Each hour's job owns every thread's records and counters for that hour, so they are dropped
    // as soon as the hour is formatted. Each mutex is taken exactly once, by the thread that claims the hour:
    // never contended.
    let mut jobs = vec![vec![Hour::default(); workers]; hours.len()];
    for (t, table) in tables.iter_mut().enumerate() {
        for x in &mut table.hours {
            let h = hours.binary_search(&x.hour).unwrap(); // before the take, which zeroes x.hour
            jobs[h][t] = std::mem::take(x);
        }
    }
    let jobs: Vec<Mutex<Vec<Hour>>> = jobs.into_iter().map(Mutex::new).collect();
    let (all, ranks) = global_ranks(&tables, sorted);

    // Merges every thread's records of one hour and formats its output lines in endpoint order (spec 4): a
    // counting sort by global endpoint rank (counts summed per rank, prefix sums, one pass placing each latency)
    // puts each group's latencies side by side in `scratch`, with no comparison sort and no byte compares.
    let format_hour = |i: usize, scratch: &mut Vec<u32>| {
        let (parts, n) = (std::mem::take(&mut *jobs[i].lock().unwrap()), all.len());
        let (mut start, mut errors) = (vec![0u32; n + 1], vec![0u64; n]);
        for (t, x) in parts.iter().enumerate() {
            for (e, &c) in x.counts.iter().enumerate().filter(|&(_, &c)| c > 0) {
                let r = ranks[t][e] as usize;
                start[r + 1] += c;
                errors[r] += x.errors[e] as u64;
            }
        }
        for r in 1..=n {
            start[r] += start[r - 1]; // start[r] is now where rank r's latencies begin
        }
        // resize keeps the scratch capacity across hours; every slot below start[n] is written by the placement.
        scratch.resize(start[n] as usize, 0);
        let mut next = start.clone();
        for (t, x) in parts.iter().enumerate() {
            let mut k = 0;
            while k < x.recs.len() {
                let (e, lat, w) = match x.recs[k] {
                    v if v & ID_ESC == ID_ESC => (x.recs[k + 1], x.recs[k + 2], 3),
                    v => (v & ID_ESC, v >> ID_BITS, 1),
                };
                let r = ranks[t][e as usize] as usize;
                scratch[next[r] as usize] = lat;
                next[r] += 1;
                k += w;
            }
        }
        drop(parts); // every latency is placed: the records' pages go back before the formatting
        let (h, mut buf) = (hours[i].to_string(), Vec::new()); // h: YYYYMMDDHH
        let text = format!("{}-{}-{}T{}:00:00Z ", &h[..4], &h[4..6], &h[6..8], &h[8..]);
        for r in (0..n).filter(|&r| start[r] < start[r + 1]) {
            let lat = &mut scratch[start[r] as usize..start[r + 1] as usize];
            let count = lat.len() as u64;
            let [p50, p95, p99] = percentiles(lat).map(u64::from);
            buf.extend_from_slice(text.as_bytes());
            buf.extend_from_slice(all[r]);
            // Spec 3.6: error_ppm = floor(errors * 1000000 / count), in 64 bits.
            for v in [count, p50, p95, p99, errors[r], errors[r] * 1_000_000 / count] {
                buf.push(b' ');
                push_u64(&mut buf, v);
            }
            buf.push(b'\n');
        }
        buf
    };

    // Threads take the next hour from a shared counter, so uneven hours still balance.
    let (next, (tx, rx)) = (AtomicUsize::new(0), mpsc::channel::<(usize, Vec<u8>)>());
    thread::scope(|s| {
        for _ in 0..workers {
            let (tx, next, hours, format_hour) = (tx.clone(), &next, &hours, &format_hour);
            s.spawn(move || {
                let mut scratch = Vec::new();
                loop {
                    let i = next.fetch_add(1, Relaxed);
                    let Some(_) = hours.get(i) else { break };
                    tx.send((i, format_hour(i, &mut scratch))).unwrap();
                }
            });
        }
        drop(tx);
        // Write each hour's buffer as soon as every earlier hour is written, overlapping the write with the
        // remaining work and freeing buffers early.
        let (mut pending, mut written) = (vec![None; hours.len()], 0);
        for (i, buf) in rx {
            pending[i] = Some(buf);
            while let Some(buf) = pending.get_mut(written).and_then(Option::take) {
                out.write_all(&buf).expect("write output");
                written += 1;
            }
        }
    });
}
