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
fn mask32(data: &[u8], i: usize, byte: u8) -> u32 {
    use std::arch::x86_64::*;
    let v = &data[i..i + 32];
    // Safety: v is 32 readable bytes and AVX2 is enabled at compile time (x86-64-v3, .cargo/config.toml).
    let v = unsafe { _mm256_loadu_si256(v.as_ptr() as *const __m256i) };
    unsafe { _mm256_movemask_epi8(_mm256_cmpeq_epi8(v, _mm256_set1_epi8(byte as i8))) as u32 }
}

/// The endpoint's first byte and the LF of the line starting at p. The method holds no '/', so the first '/'
/// from byte 32 opens the endpoint; one 32-byte load finds it and, for most lines, the LF too.
/// Measured on this VM (seed 1 full, 4 workers; a-5): scan 108 -> 79 ns per line against byte-by-byte searches
/// (an AVX2 LF search alone gave 95, adding a SWAR '/' search 86).
fn line_fields(data: &[u8], p: usize) -> (usize, usize) {
    let mut i = p + ENDPOINT_MIN;
    if i + 32 > data.len() {
        // Near the end of the input, where a 32-byte load would overrun: byte by byte.
        let ep = find(data, i, b'/');
        return (ep, find(data, ep, b'\n'));
    }
    let ep = i + mask32(data, i, b'/').trailing_zeros() as usize;
    loop {
        let lf = mask32(data, i, b'\n');
        if lf != 0 {
            return (ep, i + lf.trailing_zeros() as usize);
        }
        i += 32;
        if i + 32 > data.len() {
            return (ep, find(data, i, b'\n'));
        }
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
    use std::arch::x86_64::*;
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

fn fold_mul(a: u64, b: u64) -> u64 {
    let p = a as u128 * b as u128;
    (p as u64) ^ ((p >> 64) as u64)
}

/// Hash of (length, first 16 bytes, last 16 bytes) of the endpoint; equality is confirmed by a full compare.
fn key_hash(ep: &[u8]) -> u64 {
    let word = |s: &[u8]| u64::from_le_bytes(s.try_into().unwrap());
    let w = if ep.len() >= 16 {
        let t = ep.len() - 16;
        [0, 8, t, t + 8].map(|i| word(&ep[i..i + 8]))
    } else {
        // Short endpoints are copied into a zeroed buffer so no byte past the endpoint is read.
        let mut buf = [0u8; 16];
        buf[..ep.len()].copy_from_slice(ep);
        [word(&buf[..8]), word(&buf[8..]), 0, 0]
    };
    const K: u64 = 0x9E37_79B9_7F4A_7C15;
    w.iter().fold(ep.len() as u64, |h, &x| fold_mul(h ^ x, K))
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
    slots: Vec<(u32, u32)>, // (high hash bits, endpoint id + 1); id 0 marks an empty slot
    eps: Vec<(u32, u8)>,    // endpoint id -> (offset in `pool`, length); spec 2: at most 125 bytes
    // Each distinct endpoint's bytes, copied here when first seen. Everything after the scan reads endpoints from
    // this pool, never from the mapping, whose page-table entries are released once its range is scanned: read
    // from the mapping, the global merge and the output refault those pages. Measured on this VM with the
    // per-endpoint merge below: merge 0.69 -> 0.27 ms at 4 workers, 2.27 -> 1.62 ms at 12 (a-17).
    pool: Vec<u8>,
    hours: Vec<Hour>,
}

impl Table {
    fn hour(&mut self, hour: u32) -> usize {
        if let Some(i) = self.hours.iter().position(|h| h.hour == hour) {
            return i;
        }
        self.hours.push(Default::default());
        self.hours.last_mut().unwrap().hour = hour;
        self.hours.len() - 1
    }

    fn ep(&self, id: u32) -> &[u8] {
        &self.pool[self.eps[id as usize].0 as usize..][..self.eps[id as usize].1 as usize]
    }

    /// The local id of an endpoint, interning it on first sight; the table doubles once it is half full.
    fn endpoint(&mut self, ep: &[u8]) -> u32 {
        let (h, mask) = (key_hash(ep), self.slots.len() - 1);
        let (tag, mut i) = ((h >> 32) as u32, h as usize & mask);
        loop {
            let (t, id) = self.slots[i];
            if id == 0 {
                self.eps.push((self.pool.len() as u32, ep.len() as u8));
                self.pool.extend_from_slice(ep);
                self.slots[i] = (tag, self.eps.len() as u32);
                if self.eps.len() * 2 > self.slots.len() {
                    self.grow();
                }
                return self.eps.len() as u32 - 1;
            }
            if t == tag && self.ep(id - 1) == ep {
                return id - 1;
            }
            i = (i + 1) & mask;
        }
    }

    fn grow(&mut self) {
        let n = self.slots.len();
        let old = std::mem::replace(&mut self.slots, vec![(0, 0); n * 2]);
        for (tag, id) in old.into_iter().filter(|s| s.1 != 0) {
            let mut i = key_hash(self.ep(id - 1)) as usize & (self.slots.len() - 1);
            while self.slots[i].1 != 0 {
                i = (i + 1) & (self.slots.len() - 1);
            }
            self.slots[i] = (tag, id);
        }
    }

    fn add(&mut self, at: usize, ep: &[u8], error: bool, latency: u32) {
        let e = self.endpoint(ep);
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
    table.slots = vec![(0, 0); 1024];
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
        table.add(at, &data[ep_start..sp - 4], error, latency);
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
                // The probe table is not needed after the scan; freed here, it is not held through the tail.
                t.slots = Vec::new();
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
