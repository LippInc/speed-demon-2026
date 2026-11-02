//! Track A: per (hour, endpoint) request count, nearest-rank p50/p95/p99 latency and 5xx rate.
//! The threads claim an mmap of /data/requests.log in 1 MiB blocks, in file order, and write each line as a 32-bit
//! row into its hour's region. An hour is finished (counted, sorted, reduced to one small cell per endpoint) as soon as
//! the parsed prefix of the file reaches a line two hours later, while other threads keep parsing; only a few hours' rows
//! are alive at once. Once the input is parsed, the text goes straight into the output's pages through a shared mapping,
//! at each hour's offset, in parallel, as the cells go back to the kernel: 10 MB of text never sits beside the rows.
//! Main tricks: parse and line (a-62), the endpoint table (a-63), finish and deal (a-61), late text (a-67), the release at the end (a-71).
// An unsafe fn runs its unsafe operations bare (its callers' Safety notes cover them); a safe fn wraps each in an
// unsafe block with its own Safety note.
#![allow(unsafe_op_in_unsafe_fn)]
// No Rust runtime set-up before main: no signal stack per thread, which is five system calls each.
#![no_main]
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::*};
use std::{arch::x86_64::*, collections::*, process::Command, sync::OnceLock, thread::Scope};
use std::{fs::File, os::fd::AsRawFd, os::unix::process::CommandExt, sync::LazyLock, sync::Mutex};
use std::{ptr::copy_nonoverlapping as copy, slice::from_raw_parts, slice::from_raw_parts_mut};

// Spec 2: every timestamp lies in the same 72 hours for every seed and scale, 2026-10-30T18:00Z to 2026-11-02T17:59Z.
const HOURS: usize = 72;
const DAYS: [&str; 4] = ["2026-10-30", "2026-10-31", "2026-11-01", "2026-11-02"];
// Spec 2: exactly 1,842 distinct endpoints per seed, and 2,047 ids at most fit their 11 bits (insert asserts it).
const IDS: usize = 2048;
const ID_BITS: u32 = IDS.ilog2();
const ID_MASK: u64 = IDS as u64 - 1;
// 1 MiB: two blocks share a 2 MiB page table, so at most two threads fault in its pages under its lock at once (with 256 KiB
// blocks, eight threads did; that lock took 16-18 pct of the kernel's cycles, box 16.29 -> 15.42 ms, a-64).
const BLOCK: usize = 1 << 20;
// An hour is finished once the parsed prefix of the file reaches a line LAG hours later. Spec 2 promises no bound on how
// far a line is out of order (generated files: at most one hour behind the latest hour before it), so a line that comes
// after its hour was finished is detected, and the run starts over with no early finishing: exact for any order.
const LAG: usize = 2;
/// Slots of the endpoint table: 16,384 stay a ninth full (spec 2: 1,842 endpoints), so a probe nearly always ends at the
/// first slot, and a miss there costs more than the bigger table (a-69).
const SLOTS: usize = 16384;
/// A slot's index is the hash's top log2(SLOTS) bits.
const SLOT_SHIFT: u32 = u64::BITS - SLOTS.ilog2();
/// A row is (endpoint id + 1) << 21 | 5xx << 20 | latency, 0 for a slot no line took; a line with a latency of ESCAPE
/// or more (0.24 pct on seed 1) goes to its hour's long list instead, as latency << 32 | the row without the latency.
const ESCAPE: u32 = (1 << 20) - 1;
/// Rows a thread claims in one hour's region at a time.
const CLAIM: usize = 256;
/// Bytes of address space per region, of which only the pages touched cost memory: rows, then the sorted latencies, and
/// from the top down the hour's long list.
const STRIDE: usize = 1 << 27;
/// The timestamp's 27 bytes and the space after them: a line's method starts here.
const METHOD_AT: usize = 28;
/// From the space after the endpoint to the latency: that space, the 3-digit status, a space.
const TO_LATENCY: usize = 5;
/// The window opens at 18:00Z: hour 0 is 18:00 on the first of DAYS.
const FIRST_HOUR: usize = 18;

unsafe extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn madvise(addr: *const u8, len: usize, advice: i32) -> i32;
    fn prctl(option: i32, ...) -> i32;
    fn mallopt(param: i32, value: i32) -> i32;
    fn open(path: *const std::ffi::c_char, flags: i32, ...) -> i32;
    fn ftruncate(fd: i32, len: i64) -> i32;
}
/// mmap's protections and flags on Linux x86-64: MAP_PRIVATE 2, MAP_FIXED 0x10, MAP_ANONYMOUS 0x20, MAP_NORESERVE 0x4000.
const PROT_R: i32 = 1;
const PROT_RW: i32 = 3;
const MAP_SHARED: i32 = 1;
const MAP_PRIV_FIXED: i32 = 0x12;
const MAP_PRIV_ANON: i32 = 0x22;
/// Private, anonymous and not reserved: only the pages touched cost memory.
const MAP_ANON_LAZY: i32 = 0x4022;
/// madvise's advice: the pages go back to the kernel; file mappings keep their data in the file.
const MADV_DONTNEED: i32 = 4;

/// Byte mask of `byte` in the 32 bytes at p, one bit per byte. Safety: p..p + 32 must be readable.
unsafe fn mask32(p: *const u8, byte: u8) -> u32 {
    // AVX2 is enabled at compile time (target-cpu=alderlake, the scoring box's i5-13500).
    let v = _mm256_loadu_si256(p as _);
    _mm256_movemask_epi8(_mm256_cmpeq_epi8(v, _mm256_set1_epi8(byte as i8))) as u32
}

/// The value of 8 decimal digits, one per byte (0 to 9), the most significant in the low byte: two multiply-add steps
/// fold digit pairs, then 2-digit pairs, into two 4-digit halves.
unsafe fn digits8(v: u64) -> u64 {
    // SSE2 and SSSE3 are enabled at compile time too.
    let pairs = _mm_maddubs_epi16(_mm_cvtsi64_si128(v as i64), _mm_set1_epi16(0x010a));
    let r = _mm_cvtsi128_si64(_mm_madd_epi16(pairs, _mm_set1_epi32(0x0001_0064))) as u64;
    (r & 0xffff_ffff) * 10_000 + (r >> 32)
}

/// The endpoint's start and length and the latency of the line at p, and the next line's start (spec 2:
/// "<timestamp> <method> <endpoint> <status> <latency>", the timestamp always 27 bytes). A 64-byte space mask from
/// the method on holds the space after the method (at most 7 bytes) and, unless the endpoint is long, the one after it;
/// the 3-digit status follows, its first digit '5' exactly for a 5xx, then the latency, whose digits one 8-byte load
/// finds up to its LF. Read forward, the next line's start comes out of the latency's length: no LF search first.
/// Safety: d + p..d + p + 128 must be readable, and so must the line's end + 32.
unsafe fn line(d: *const u8, p: usize) -> (usize, usize, u32, usize) {
    let mut at = p + METHOD_AT;
    let spaces = mask32(d.add(at), b' ') as u64 | (mask32(d.add(at + 32), b' ') as u64) << 32;
    let ep = at + 1 + spaces.trailing_zeros() as usize;
    // The space after the endpoint; past the first 64 bytes, 32 at a time (bits stay relative to `at`).
    let mut rest = spaces & spaces.wrapping_sub(1);
    while rest == 0 {
        at += 32;
        rest = (mask32(d.add(at + 32), b' ') as u64) << 32;
    }
    let end = at + rest.trailing_zeros() as usize;
    let digits = (d.add(end + TO_LATENCY) as *const u64).read_unaligned() ^ 0x3030_3030_3030_3030;
    // After the xor a byte is a digit exactly when it is below 10; adding 0x76 sets the top bit of every other one.
    let other = (digits.wrapping_add(0x7676_7676_7676_7676) | digits) & 0x8080_8080_8080_8080;
    let n = other.trailing_zeros() as usize / 8;
    // n digits, then the LF; with no other byte in the load n is 8, and any rest comes one by one (spec 2: an
    // unsigned 32-bit value, so at most 10 digits, and the u32 below holds it).
    let mut latency = digits8(digits << (64 - 8 * n));
    let mut q = end + TO_LATENCY + n;
    while n == 8 && *d.add(q) != b'\n' {
        latency = latency * 10 + (*d.add(q) - b'0') as u64;
        q += 1;
    }
    (ep, end - ep, latency as u32, q + 1)
}

/// A 64-bit hash of the whole len-byte endpoint at p: its first and last 32 bytes, each cut to the endpoint, and from 65
/// bytes on the two loads over its middle (spec 2: at most 125 bytes, so four loads cover it), mixed by AES rounds that
/// work on both 16-byte halves at once, the length a round key. Two rounds follow the last input, which spreads every
/// input bit over the whole half. Safety: p - 32..p + 128 must be readable. It is: an endpoint starts at byte 32 or later
/// (a 27-byte timestamp, a method of 3 to 7 bytes, two spaces; spec 2), and the mapping has 4 KiB of zeros after the input.
unsafe fn ep_hash(p: *const u8, len: usize) -> u64 {
    let load = |at: *const u8| _mm256_loadu_si256(at as _);
    // The lane numbers 0 to 31 (a constant once compiled) and the length, to cut a load to the endpoint's bytes.
    let iota = load(std::array::from_fn::<u8, 32, _>(|i| i as u8).as_ptr());
    let n = _mm256_set1_epi8(len as i8);
    // Byte i of the first load is the endpoint's when i < n, of the last load when 31 - i < n.
    let first = _mm256_and_si256(load(p), _mm256_cmpgt_epi8(n, iota));
    let last_in = _mm256_cmpgt_epi8(n, _mm256_sub_epi8(_mm256_set1_epi8(31), iota));
    let mut x = _mm256_aesenc_epi128(first, _mm256_and_si256(load(p.add(len).sub(32)), last_in));
    if len > 64 {
        x = _mm256_aesenc_epi128(x, load(p.add(32)));
        x = _mm256_aesenc_epi128(x, load(p.add(len - 64)));
    }
    x = _mm256_aesenc_epi128(x, _mm256_zextsi128_si256(_mm_cvtsi64_si128(len as i64)));
    // The last round key has no structure to cancel with the input (Knuth's MMIX multiplier; any odd constant would do).
    x = _mm256_aesenc_epi128(x, _mm256_set1_epi64x(0x5851_f42d_4c95_7f2d));
    let x = _mm_xor_si128(_mm256_castsi256_si128(x), _mm256_extracti128_si256::<1>(x));
    _mm_cvtsi128_si64(x) as u64
}

/// An endpoint's bytes, where the line that first had it lies in the input's mapping: 128 bytes can be read from its start.
type Name = &'static [u8];
/// An hour's result for one endpoint: count, p50, p95, p99, errors, error_ppm (count 0: no line).
type Cell = [u32; 6];
/// An hour's place in the output: where its text starts and how long it is.
#[derive(Clone, Copy)]
struct Place {
    offset: usize,
    size: usize,
}
/// A thread's open chunk of CLAIM rows in an hour's region: the next free row and the chunk's end.
#[derive(Clone, Copy)]
struct Chunk {
    at: *mut u32,
    end: *mut u32,
}

/// The run's shared progress, under one lock. Each thread takes, in this order: an hour that is safe to finish, else the
/// next block, at most two per thread past the oldest unparsed one, so few rows wait; else, once the input is parsed, the
/// text of the next hour whose place is known; else it spins a moment and looks again (a sleep costs a wake-up, about
/// 130 us on the box, an idle state's exit, PERF.md "Floors and budgets"). Every 8th look it does sleep: its CPU goes
/// idle, and an idle CPU takes over a thread that another process has pushed off its own (a-70: 22.0 -> 15.1 ms beside
/// one busy process, with the holder that a-71 took out; this code reads 16; spinning threads leave that thread waiting
/// for its turn, 3 ms or more). Measured effect: PERF.md a-55, a-67, a-69, a-70.
#[derive(Default)]
struct State {
    next_block: usize,
    parsed: BTreeSet<usize>,       // parsed blocks past the watermark
    watermark: usize,              // every block below it is parsed
    next_hour: usize,              // every hour below it is claimed for finishing
    late: bool,                    // a block brought lines of an hour already claimed
    sizes: BTreeMap<usize, usize>, // finished hours waiting for the ones before them: the text's size
    places: Vec<Place>,            // the hours placed so far
    next_text: usize,              // every hour below it is claimed for its text
    front: usize,                  // the hour of the first line at the watermark
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(Mutex::default);
/// Every hour's region of rows, from a pool filled at the start: an hour's region goes back when it is finished and a
/// later hour takes it, pages already faulted in (a fresh region per hour cost 7.8K more page faults, 6.1K -> 13.9K, a-59).
static REGIONS: [OnceLock<usize>; HOURS] = [const { OnceLock::new() }; HOURS];
static POOL: Mutex<Vec<usize>> = Mutex::new(Vec::new());
/// Threads claim CLAIM rows at a time, so a row needs no atomic.
static CLAIMED: [AtomicUsize; HOURS] = [const { AtomicUsize::new(0) }; HOURS];
/// How many lines of each hour are on its long list.
static LONG: [AtomicUsize; HOURS] = [const { AtomicUsize::new(0) }; HOURS];
/// How far each range has been given back at the end of the run: the input past its first 8 MiB, those 8 MiB, the output.
static RELEASED: [AtomicUsize; 3] = [const { AtomicUsize::new(0) }; 3];
/// Every endpoint met so far, one table for all threads, open addressing with linear probing. A slot is the hash's top
/// 53 bits, the lowest of them forced set, and the id + 1 in the low 11, 0 when empty: the hash alone is an endpoint's identity, and for 1,842 of them
/// the chance that two share those bits is about 4e-10. Slots are only filled, under the lock of NAMES, so a slot seen
/// empty there ends the probe; the table fills in the run's first moments and is then only read, one copy in every core's
/// cache. A slot carries its id, so its load needs no ordering.
static TABLE: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
/// The names, by id, pushed under the lock into room for IDS made up front, so the Vec never moves and a name, once
/// pushed, is never written again: readers keep a pointer to them. Then the ids in byte order, as last asked for.
static NAMES: Mutex<(Vec<Name>, Vec<u16>)> = Mutex::new((Vec::new(), Vec::new()));

/// A new endpoint for slot i, unless another thread just filled it.
#[cold]
fn insert(i: usize, h: u64, p: *const u8, len: usize) {
    let mut names = NAMES.lock().unwrap();
    if TABLE[i].load(Relaxed) == 0 {
        // An id + 1 must fit its 11 bits and the Vec must not move (spec 2: 1,842 endpoints).
        assert!(names.0.len() + 1 < IDS);
        // Safety: the endpoint is the len bytes at p, in the input's mapping, which is never unmapped.
        names.0.push(unsafe { from_raw_parts(p, len) });
        TABLE[i].store(h & !ID_MASK | names.0.len() as u64, Relaxed);
    }
}

/// The id of the len-byte endpoint at p, a new one the first time it is met. Safety: p - 32..p + 128 must be readable.
unsafe fn id(p: *const u8, len: usize) -> u32 {
    // Bit ID_BITS set: no hash matches an empty slot.
    let h = ep_hash(p, len) | 1 << ID_BITS;
    let mut i = (h >> SLOT_SHIFT) as usize;
    loop {
        match TABLE[i].load(Relaxed) {
            s if s >> ID_BITS == h >> ID_BITS => return (s & ID_MASK) as u32 - 1,
            0 => insert(i, h, p, len),
            _ => i = (i + 1) % SLOTS,
        }
    }
}

/// The id of every endpoint met, in byte order (spec 4), and the names by id: the ids new since the last call are
/// appended and sorted in, a stable sort that finds the sorted prefix as one run.
fn order() -> (Vec<u16>, *const Name) {
    let mut guard = NAMES.lock().unwrap();
    let (names, order) = &mut *guard;
    if order.len() < names.len() {
        order.extend(order.len() as u16..names.len() as u16);
        order.sort_by_key(|&id| names[id as usize]);
    }
    (order.clone(), names.as_ptr())
}

/// Hour h's region: the first to ask takes one from the pool (72 regions for 72 hours, so it never runs out).
fn region(h: usize) -> *mut u32 {
    *REGIONS[h].get_or_init(|| POOL.lock().unwrap().pop().unwrap()) as *mut u32
}

/// Block b's first line start: the first at or after b * BLOCK, or the input's end when no LF is left (spec 2: the
/// last line ends with an LF); 0 for block 0.
fn align(data: &[u8], b: usize) -> usize {
    let n = (b * BLOCK).min(data.len());
    if n == 0 {
        return 0;
    }
    let lf = data[n - 1..].iter().position(|&c| c == b'\n');
    lf.map_or(data.len(), |o| n + o)
}

/// The hour of the line at p: hours since 2026-10-30T18, from "YYYY-MM-DDTHH" (spec 2: the window crosses two midnights
/// and the October to November boundary): days 10-30 and 10-31 are 0 and 1, 11-01 and 11-02 are 2 and 3, which is the
/// day's ones digit plus the month's. Past the input's end (zeros) it is some large number.
fn hour_at(data: &[u8], p: usize) -> usize {
    // Safety: the mapping has 4 KiB of zeros after the input.
    let digit = |i: usize| unsafe { *data.as_ptr().add(p + i) }.wrapping_sub(b'0') as usize;
    ((digit(9) + digit(6)) * 24 + digit(11) * 10 + digit(12)).wrapping_sub(FIRST_HOUR)
}

/// Writes a row for every line starting in data[start..end] into its hour's region, through cur, this thread's open
/// chunk per hour, and returns the lowest hour met. Reads may pass the input's end by up to 128
/// bytes (main maps zeros there) and start up to 32 bytes before an endpoint, inside its line.
fn parse(data: &[u8], start: usize, end: usize, cur: &mut [Chunk; HOURS]) -> usize {
    let d = data.as_ptr();
    let (mut prev_prefix, mut p, mut h, mut low) = (u64::MAX, start, 0, HOURS);
    let mut chunk = cur[0]; // hour h's chunk (h starts at 0)
    while p < end {
        // Lines usually share the previous line's hour, so compare it first: spec 2's year is always 2026, so bytes
        // 5..13 ("MM-DDTHH") name the hour, one u64 load (a-56). Two cache lines 1,280 bytes ahead are asked for meanwhile: the
        // prefetcher stops at each 4 KiB page (box: 18.12 -> 17.69 ms).
        // Safety: a line has at least 40 bytes; a prefetch never faults.
        unsafe { _mm_prefetch::<_MM_HINT_T0>(d.add(p + 1280) as _) };
        unsafe { _mm_prefetch::<_MM_HINT_T0>(d.add(p + 1344) as _) };
        let prefix = unsafe { (d.add(p + 5) as *const u64).read_unaligned() };
        if prefix != prev_prefix {
            cur[h] = chunk;
            h = hour_at(data, p);
            (chunk, low, prev_prefix) = (cur[h], low.min(h), prefix);
        }
        // Safety: what line reads (its doc: 128 bytes from the start, the line's end + 32) is inside the input or its zero tail.
        let (ep, len, latency, next) = unsafe { line(d, p) };
        // Safety: as for line; an endpoint starts at byte 32 or later of its line (ep_hash).
        let row = (unsafe { id(d.wrapping_add(ep), len) } + 1) << 21;
        // Safety: the status follows the endpoint, its first digit '5' exactly for a 5xx.
        let row = row | ((unsafe { *d.add(ep + len + 1) } == b'5') as u32) << 20;
        if latency >= ESCAPE {
            let i = LONG[h].fetch_add(1, Relaxed);
            let top = region(h).wrapping_add(STRIDE / 4) as *mut u64;
            // Safety: the region's top holds them (STRIDE is far more than two rows per line and 8 bytes per long one).
            unsafe { top.sub(i + 1).write((latency as u64) << 32 | row as u64) };
        } else {
            if chunk.at == chunk.end {
                // A new chunk of CLAIM rows. A pooled region holds an older hour's bytes, and a row no line
                // takes must read 0, so the chunk is zeroed first.
                let at = region(h).wrapping_add(CLAIMED[h].fetch_add(CLAIM, Relaxed));
                // Safety: the chunk lies inside the region (STRIDE holds two rows per line and a chunk per thread).
                unsafe { at.write_bytes(0, CLAIM) };
                let end = at.wrapping_add(CLAIM);
                chunk = Chunk { at, end };
            }
            // Safety: chunk.at < chunk.end.
            unsafe { *chunk.at = row | latency };
            chunk.at = chunk.at.wrapping_add(1);
        }
        p = next;
    }
    cur[h] = chunk;
    low
}

/// Hour h's cells, one per endpoint with lines, and the size of its text. The rows are counted per endpoint, sorted by
/// latency in two counting passes and dealt to each endpoint's span, so every span is sorted and p50, p95 and p99 are
/// read off by rank. Not inlined: its arrays stay out of work's frame, so that text can give their pages back.
#[inline(never)]
fn finish(h: usize, cells: *mut Cell) -> usize {
    // order() also sorts in the ids new since the last call, here and not in text (PERF a-70: 13.52 against 13.29-13.33 ms).
    let (_, names) = order();
    let claimed = CLAIMED[h].load(Relaxed);
    // Safety: its rows are all written (their blocks are parsed) and no other thread touches the region until it goes
    // back to the pool. A late line is the exception, and the run is then repeated.
    let n = LONG[h].load(Relaxed);
    let below_long = STRIDE / 4 - 2 * n;
    let (rows, values) = unsafe { from_raw_parts_mut(region(h), below_long) }.split_at_mut(claimed);
    // Safety: the top n u64 of the region, written by parse.
    let long = unsafe { from_raw_parts_mut((region(h).add(STRIDE / 4) as *mut u64).sub(n), n) };
    // Counts by (id + 1) << 1 | 5xx, where a free slot counts at 0, then by latency bits 0-7 (256 entries) and 8-19 (4,096).
    let (mut count, mut low, mut high) = ([0u32; 2 * IDS], [0u32; 1 << 8], [0u32; 1 << 12]);
    for &r in rows.iter() {
        count[(r >> 20) as usize] += 1;
        low[(r & 255) as usize] += 1;
        high[(r >> 8 & 4095) as usize] += 1;
    }
    long.iter()
        .for_each(|&p| count[(p as u32 >> 20) as usize] += 1);
    // By id + 1; start[0] is a trash slot past every span, where the free slots go.
    let (mut start, mut sum) = ([0u32; IDS], 0);
    for k in 1..IDS {
        (start[k], sum) = (sum, sum + count[2 * k] + count[2 * k + 1]);
    }
    start[0] = sum;
    let mut next = start;
    // Sorted by latency (values is scratch here), then dealt to the spans in that order: each span is sorted, the
    // long latencies (all ESCAPE or more) last, sorted among themselves.
    deal(rows, values, &mut low, 0);
    deal(&values[..claimed], rows, &mut high, 8);
    long.sort_unstable();
    let short = rows.iter().map(|&r| (r >> 21, r & ESCAPE));
    let long = long.iter().map(|&p| (p as u32 >> 21, (p >> 32) as u32));
    short.chain(long).for_each(|(k, v)| {
        let k = k as usize;
        // Safety: an endpoint's slots run from its start to its start plus its count, all below sum, and the trash
        // slot is sum, all inside values.
        unsafe { *values.get_unchecked_mut(next[k] as usize) = v };
        next[k] += (k != 0) as u32;
    });
    let mut size = 0;
    for k in 1..IDS {
        let (n, errors) = (count[2 * k] + count[2 * k + 1], count[2 * k + 1]);
        if n > 0 {
            // Spec 3.4: 0-based index of rank ceil(p * n / 100).
            let at = |p: usize| values[start[k] as usize + (p * n as usize + 99) / 100 - 1];
            // Spec 3.6: error_ppm = floor(errors * 1000000 / count), in 64 bits.
            let ppm = (errors as u64 * 1_000_000 / n as u64) as u32;
            let cell = [n, at(50), at(95), at(99), errors, ppm];
            // Safety: hour h's cells are this thread's; the names hold every id and never move or change (see NAMES).
            unsafe { *cells.add(h * IDS + k - 1) = cell };
            let len = unsafe { *names.add(k - 1) }.len();
            // 20 bytes of label, spaces and the LF, then a space and the digits of each number.
            let width = |v: &u32| (v | 1).ilog10() as usize + 2;
            size += 22 + len + cell.iter().map(width).sum::<usize>();
        }
    }
    size
}

/// Hour h's text at its place in the output, from its cells, which then go back to the kernel. Stores pass a line's end
/// (the next line covers them), so near the hour's end, next to another hour's text, a line goes through a small buffer.
fn text(h: usize, Place { offset, size }: Place, out: *mut u8, cells: *mut Cell) {
    let (order, names) = order();
    // 20 bytes, then spaces to 32 so it can be copied whole: byte 20 stays the space after the label, and the
    // name, copied from byte 21, covers the rest.
    let (day, hour) = (DAYS[(h + FIRST_HOUR) / 24], (h + FIRST_HOUR) % 24);
    let label = format!("{day}T{hour:02}:00:00Z            ");
    let mut tmp = [0u8; 512];
    // finish's arrays give their pages back: finish and text are called from the same frame in work, and
    // finish is not inlined, so its arrays (about 49 KiB) lie in the 128 KiB below this frame. If they did not, only the peak
    // would suffer: nothing in that range is in use (tmp is this frame's only array, so the frame ends within the page below
    // tmp, and text calls nothing while the range goes).
    let below = (tmp.as_ptr() as usize & !4095) - 4096;
    unsafe { madvise((below - (1 << 17)) as *const u8, 1 << 17, MADV_DONTNEED) };
    // Safety: the output is mapped past every hour's place, the names as in finish; each copy reads only its own
    // source's bytes (label 32, name 128), and a line and its stores take at most 300 bytes.
    unsafe {
        let (mut w, end, buf) = (out.add(offset), out.add(offset + size), tmp.as_mut_ptr());
        let cells = cells.add(h * IDS);
        for &id in &order {
            let cell = *cells.add(id as usize);
            if cell[0] > 0 {
                let name = *names.add(id as usize);
                let t = if w.add(300) <= end { w } else { buf };
                copy(label.as_ptr(), t, 32);
                copy(name.as_ptr(), t.add(21), 128);
                let e = cell.iter().fold(t.add(21 + name.len()), |e, &v| put(e, v));
                *e = b'\n';
                let len = e.offset_from(t) as usize + 1;
                if t != w {
                    copy(t, w, len);
                }
                w = w.add(len);
            }
        }
        madvise(cells as *const u8, IDS * size_of::<Cell>(), MADV_DONTNEED);
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

/// What every thread of a run works on: the input, the output's and the cells' mappings as addresses (a raw pointer is
/// not Send), the number of threads, and how many hours past an hour the parsed prefix must be before it is finished.
#[derive(Clone, Copy)]
struct Job {
    data: &'static [u8],
    out: usize,
    cells: usize,
    workers: usize,
    lag: usize,
}

/// One of the run's threads: it takes jobs from State, in the order State's comment gives, until all is claimed, and then
/// shares the release of the mappings.
fn work(job: Job) {
    let (data, out, cells) = (job.data, job.out, job.cells);
    let nblocks = data.len().div_ceil(BLOCK);
    // Two nulls read as at == end, so a thread's first row of an hour claims a chunk.
    let (at, end) = (std::ptr::null_mut(), std::ptr::null_mut());
    let mut cur = [Chunk { at, end }; HOURS];
    let mut idle = 0;
    let mut s = STATE.lock().unwrap();
    while !s.late && s.next_text < HOURS {
        let parsed = s.watermark == nblocks;
        if s.next_hour < HOURS && (parsed || s.next_hour + job.lag <= s.front) {
            let h = s.next_hour;
            s.next_hour += 1;
            drop(s);
            let size = finish(h, cells as *mut Cell);
            s = STATE.lock().unwrap();
            let st = &mut *s;
            st.sizes.insert(h, size);
            // places.len() is the next hour to place; a size for it means every hour before it is placed.
            while let Some(size) = st.sizes.remove(&st.places.len()) {
                let offset = st.places.last().map_or(0, |p| p.offset + p.size);
                st.places.push(Place { offset, size });
            }
            // The region goes back. Once the input is parsed no hour takes one, so the pages go back to the kernel,
            // before the text's pages come; the thread that parsed the last block empties the pool.
            let r = region(h) as *const u8;
            let parsed = st.watermark == nblocks;
            if !parsed {
                POOL.lock().unwrap().push(r as usize);
            }
            drop(s);
            if parsed {
                // Safety: nothing reads the region now.
                unsafe { madvise(r, STRIDE, MADV_DONTNEED) };
            }
        } else if s.next_block < nblocks && s.next_block < s.watermark + 2 * job.workers {
            let b = s.next_block;
            s.next_block += 1;
            drop(s);
            let low = parse(data, align(data, b), align(data, b + 1), &mut cur);
            // A line of an hour already claimed marks the run late.
            s = STATE.lock().unwrap();
            let st = &mut *s;
            st.late |= low < st.next_hour;
            st.parsed.insert(b);
            while st.parsed.remove(&st.watermark) {
                st.watermark += 1;
                st.front = hour_at(data, align(data, st.watermark));
            }
            let all = st.watermark == nblocks;
            drop(s);
            if all {
                for &r in POOL.lock().unwrap().iter() {
                    // Safety: nothing reads a pooled region.
                    unsafe { madvise(r as *const u8, STRIDE, MADV_DONTNEED) };
                }
            }
        } else if parsed && s.next_text < s.places.len() {
            let h = s.next_text;
            s.next_text += 1;
            let place = s.places[h];
            drop(s);
            text(h, place, out as *mut u8, cells as *mut Cell);
        } else {
            drop(s);
            idle += 1;
            if idle % 8 == 0 {
                std::thread::sleep(std::time::Duration::from_micros(1));
            }
            (0..64).for_each(|_| std::hint::spin_loop());
        }
        s = STATE.lock().unwrap();
    }
    let end = s.places.last().map_or(0, |p| p.offset + p.size);
    drop(s);
    // All is claimed. Left to the exit, tearing down these mappings runs on one CPU, inside the timed run (a-71), so the
    // threads give the pages back in shared steps. A page still in use just faults in again: so the input's first 8 MiB,
    // where the names are, go last, and the output's last 2 MiB, which may still be being written, stay.
    let give_back = |i: usize, start: *const u8, len: usize, step: usize| {
        let mut at = RELEASED[i].fetch_add(step, Relaxed);
        while at < len {
            // Safety: file mappings: the data stays in the files.
            unsafe { madvise(start.add(at), step.min(len - at), MADV_DONTNEED) };
            at = RELEASED[i].fetch_add(step, Relaxed);
        }
    };
    let (start, len, step) = (data.as_ptr(), data.len(), 8 << 20);
    give_back(0, start.wrapping_add(step), len.saturating_sub(step), step);
    give_back(1, start, len.min(step), step);
    give_back(2, out as *const u8, end.saturating_sub(2 << 20), 1 << 20);
}

/// Thread i of a run starts threads 2i and 2i + 1: the last one is up after five spawns, not eleven.
fn spawn_tree<'s, 'e>(s: &'s Scope<'s, 'e>, i: usize, job: Job) {
    for c in [2 * i, 2 * i + 1].into_iter().filter(|&c| c <= job.workers) {
        s.spawn(move || spawn_tree(s, c, job));
    }
    work(job);
}

/// Maps the input, the output and the working memory, runs the threads, cuts the output to its size. The C runtime calls
/// it directly (no_main above).
#[unsafe(no_mangle)]
extern "C" fn main() -> i32 {
    // One malloc arena (M_ARENA_MAX -8): the threads allocate little, and each arena costs pages.
    unsafe { mallopt(-8, 1) };
    // No transparent huge pages (PR_SET_THP_DISABLE 41): with THP on, an hour's first row would charge 2 MiB.
    unsafe { prctl(41, 1usize, 0usize, 0usize, 0usize) };
    // Safety, for this function's system calls: they take constants, descriptors and mappings made here, no Rust data.
    let map = |len, prot, flags, fd| {
        let p = unsafe { mmap(std::ptr::null_mut(), len, prot, flags, fd, 0) };
        assert_ne!(p as isize, -1, "mmap failed");
        p
    };
    let file = File::open("/data/requests.log").expect("open input");
    let len = file.metadata().expect("stat input").len() as usize;
    // Map the file read-only over an anonymous reservation 4 KiB longer, so the bytes past the input read as zeros: the
    // kernel zero-fills the file's last page and the reservation is zeros. An empty log (spec 3.7: an empty summary) maps nothing over the
    // reservation (a zero-length mmap fails), which reads as no lines.
    let room = map(len + 4096, PROT_R, MAP_PRIV_ANON, -1);
    let input = unsafe { mmap(room, len, PROT_R, MAP_PRIV_FIXED, file.as_raw_fd(), 0) };
    assert!(len == 0 || input == room, "cannot map /data/requests.log");
    // Safety: room..room + len is mapped read-only and never unmapped.
    let data: &'static [u8] = unsafe { from_raw_parts(room, len) };
    // O_RDWR | O_CREAT | O_TRUNC: the text goes through a shared mapping. Its
    // size is known only at the end: mapped at twice the input's (a line is at most 1.5 times an input line), then cut.
    let fd = unsafe { open(c"/out/summary.txt".as_ptr(), 0o1102, 0o644) };
    assert!(fd >= 0, "cannot open /out/summary.txt");
    let sized = unsafe { ftruncate(fd, 2 * len as i64 + 4096) };
    assert_eq!(sized, 0, "cannot size /out/summary.txt");
    let out = map(2 * len + 4096, PROT_RW, MAP_SHARED, fd) as usize;
    let base = map(STRIDE * HOURS, PROT_RW, MAP_ANON_LAZY, -1) as usize;
    let regions = (0..HOURS).map(|i| base + i * STRIDE);
    POOL.lock().unwrap().extend(regions);
    let cells = map(HOURS * IDS * size_of::<Cell>(), PROT_RW, MAP_ANON_LAZY, -1) as usize;
    NAMES.lock().unwrap().0.reserve(IDS);
    // The redo below runs with an argument: no hour is finished early.
    let redo = std::env::args().len() > 1;
    let job = Job {
        data,
        out,
        cells,
        workers: std::thread::available_parallelism().map_or(1, |n| n.get()),
        lag: if redo { HOURS } else { LAG },
    };
    std::thread::scope(|s| spawn_tree(s, 1, job));
    let st = STATE.lock().unwrap();
    if st.late {
        // A line came after its hour was finished. Nothing is written yet: again, with no hour finished before every
        // block is parsed, so the output is exact for any line order. exec returns only if it failed.
        let exec_failed = Command::new("/proc/self/exe").arg("redo").exec();
        panic!("cannot start the run again: {exec_failed}");
    }
    let last = st.places[HOURS - 1];
    let cut = unsafe { ftruncate(fd, (last.offset + last.size) as i64) };
    assert_eq!(cut, 0, "cannot cut /out/summary.txt");
    // Everything is written; exit without freeing what the kernel reclaims anyway.
    std::process::exit(0);
}
