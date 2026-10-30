//! Track A: per (hour, endpoint) request count, nearest-rank p50/p95/p99 latency and 5xx rate.
//! One pass over an mmap of /data/requests.log, split into byte ranges, one private table per thread;
//! the merge, percentile selection and formatting then run in parallel, one hour per task.

use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};
use std::os::fd::AsRawFd;
use std::thread;

const INPUT: &str = "/data/requests.log";
const OUTPUT: &str = "/out/summary.txt";

// Spec 2 grammar: ts (27 bytes), SP, method (3 to 7 bytes), SP: the endpoint starts at byte 32 to 36 of a line.
const ENDPOINT_MIN: usize = 32;

// The line search below uses AVX2, part of the x86-64-v3 baseline set in .cargo/config.toml.
#[cfg(not(target_feature = "avx2"))]
compile_error!("build with -C target-cpu=x86-64-v3");
// Spec 2 grammar: ts = "YYYY-MM-DDTHH..."; bytes 0..13 name the UTC hour.
const HOUR_PREFIX: usize = 13;

mod sys {
    use std::ffi::c_void;
    pub const PROT_READ: i32 = 1;
    pub const MAP_PRIVATE: i32 = 2;
    pub const MADV_WILLNEED: i32 = 3;
    pub const MADV_DONTNEED: i32 = 4;
    unsafe extern "C" {
        pub fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut c_void;
        pub fn madvise(addr: *mut c_void, len: usize, advice: i32) -> i32;
    }
}

/// Maps the whole file read-only; the caller guarantees len > 0 (mmap of length 0 fails).
fn map_file(file: &File, len: usize) -> &'static [u8] {
    unsafe {
        let p = sys::mmap(std::ptr::null_mut(), len, sys::PROT_READ, sys::MAP_PRIVATE, file.as_raw_fd(), 0);
        assert!(p as isize != -1, "mmap failed");
        // Ask the kernel to start reading ahead the whole range; a hint, so its result is ignored.
        sys::madvise(p, len, sys::MADV_WILLNEED);
        // The mapping is never unmapped, so the slice lives for the whole run.
        std::slice::from_raw_parts(p as *const u8, len)
    }
}

/// Drops the page-table entries of data[start..end] once its scan is done, so the exit has fewer to tear down.
/// The mapping is private and never written: a later read of these bytes (an endpoint the tail prints)
/// faults the same bytes back in from the page cache. Only whole pages inside the range are released.
/// Measured on this VM (seed 1 full, 20 runs): output closed -> process reaped 1.9-2.0 -> 0.8-1.3 ms at 1, 4
/// and 12 workers; the work before it unchanged within noise.
fn release_range(data: &[u8], start: usize, end: usize) {
    const PAGE: usize = 4096;
    let (lo, hi) = (start.div_ceil(PAGE) * PAGE, end / PAGE * PAGE);
    if lo < hi {
        // Safety: [lo, hi) lies inside the mapping, which nothing writes.
        unsafe { sys::madvise(data.as_ptr().add(lo) as *mut std::ffi::c_void, hi - lo, sys::MADV_DONTNEED) };
    }
}

/// Worker count. SD_WORKERS is a test-only override (rule 30: the hash runs force 1, 4, 12 and 24);
/// the board never sets it, so the scored run sizes itself from the CPUs it may use.
fn worker_count() -> usize {
    if let Some(n) = std::env::var("SD_WORKERS").ok().and_then(|v| v.trim().parse::<usize>().ok()) {
        return n.max(1);
    }
    thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

/// Splits data into `parts` ranges whose starts are line starts; a line belongs to the range holding its first byte.
fn line_ranges(data: &[u8], parts: usize) -> Vec<(usize, usize)> {
    let len = data.len();
    let mut starts = vec![0usize];
    for i in 1..parts {
        let nominal = len / parts * i;
        // First line start at or after `nominal`: 0 or a position right after an LF.
        let start = match nominal {
            0 => 0,
            _ => data[nominal - 1..].iter().position(|&b| b == b'\n').map_or(len, |off| nominal + off),
        };
        starts.push(start.max(*starts.last().unwrap()));
    }
    starts.push(len);
    starts.windows(2).map(|w| (w[0], w[1])).collect()
}

fn digit(b: u8) -> u32 {
    let d = b.wrapping_sub(b'0');
    assert!(d < 10, "expected a digit");
    d as u32
}

/// "YYYY-MM-DDTHH" as the decimal number YYYYMMDDHH: fits u32 and sorts like the text.
fn parse_hour(p: &[u8]) -> u32 {
    let n = |r: std::ops::Range<usize>| r.fold(0u32, |acc, i| acc * 10 + digit(p[i]));
    ((n(0..4) * 100 + n(5..7)) * 100 + n(8..10)) * 100 + n(11..13)
}

/// Unsigned decimal that must fit u32 (spec 2: latency is 0..=4294967295).
fn parse_u32(s: &[u8]) -> u32 {
    s.iter()
        .try_fold(0u32, |acc, &b| acc.checked_mul(10)?.checked_add(digit(b)))
        .expect("latency overflows u32")
}

/// Byte mask of `byte` in data[i..i + 32], one bit per byte.
fn mask32(data: &[u8], i: usize, byte: u8) -> u32 {
    use std::arch::x86_64::*;
    let v = &data[i..i + 32];
    // Safety: v is 32 readable bytes and AVX2 is enabled at compile time.
    unsafe {
        let v = _mm256_loadu_si256(v.as_ptr() as *const __m256i);
        _mm256_movemask_epi8(_mm256_cmpeq_epi8(v, _mm256_set1_epi8(byte as i8))) as u32
    }
}

/// The endpoint's first byte and the LF of the line starting at p. The method holds no '/', so the first '/'
/// from byte 32 opens the endpoint; one 32-byte load finds it and, for most lines, the LF too.
/// Measured on this VM (seed 1 full, 4 workers): scan 108 -> 79 ns per line against byte-by-byte searches
/// (an AVX2 LF search alone gave 95, adding a SWAR '/' search 86).
fn line_fields(data: &[u8], p: usize) -> (usize, usize) {
    let mut i = p + ENDPOINT_MIN;
    if i + 32 > data.len() {
        // Near the end of the input, where a 32-byte load would overrun: byte by byte.
        let ep = i + data[i..].iter().position(|&b| b == b'/').expect("no endpoint");
        return (ep, ep + data[ep..].iter().position(|&b| b == b'\n').unwrap_or(data.len() - ep));
    }
    let ep = i + mask32(data, i, b'/').trailing_zeros() as usize;
    loop {
        let lf = mask32(data, i, b'\n');
        if lf != 0 {
            return (ep, i + lf.trailing_zeros() as usize);
        }
        i += 32;
        if i + 32 > data.len() {
            return (ep, i + data[i..].iter().position(|&b| b == b'\n').unwrap_or(data.len() - i));
        }
    }
}

/// The space before the latency, the last field of the line ending at lf: the highest space among the 16 bytes
/// before the LF (spec 2: latency is at most 10 digits, and every line is at least 40 bytes, so the load stays
/// inside the line). Measured on this VM, seed 1 full, against walking back byte by byte: scan 86 -> 80 ns per
/// line at 4 workers, 82 -> 81 at 1 (the only part tried in a-6 that paid).
fn latency_space(data: &[u8], lf: usize) -> usize {
    use std::arch::x86_64::*;
    let v = &data[lf - 16..lf];
    // Safety: v is 16 readable bytes and SSE2 is part of every x86-64 baseline.
    let spaces = unsafe { _mm_movemask_epi8(_mm_cmpeq_epi8(_mm_loadu_si128(v.as_ptr() as *const __m128i), _mm_set1_epi8(b' ' as i8))) };
    assert!(spaces != 0, "no space before the latency");
    lf - 16 + (31 - (spaces as u32).leading_zeros()) as usize
}

fn fold_mul(a: u64, b: u64) -> u64 {
    let p = a as u128 * b as u128;
    (p as u64) ^ ((p >> 64) as u64)
}

/// Hash of (hour, length, first 16 bytes, last 16 bytes) of the endpoint; equality is confirmed by a full compare.
fn key_hash(hour: u32, ep: &[u8]) -> u64 {
    let word = |s: &[u8]| u64::from_le_bytes(s.try_into().unwrap());
    let (a, b, c, d) = if ep.len() >= 16 {
        let tail = &ep[ep.len() - 16..];
        (word(&ep[..8]), word(&ep[8..16]), word(&tail[..8]), word(&tail[8..]))
    } else {
        // Short endpoints are copied into a zeroed buffer so no byte past the endpoint is read.
        let mut buf = [0u8; 16];
        buf[..ep.len()].copy_from_slice(ep);
        (word(&buf[..8]), word(&buf[8..]), 0, 0)
    };
    const K: u64 = 0x9E37_79B9_7F4A_7C15;
    let h = fold_mul((hour as u64) << 16 ^ ep.len() as u64 ^ a, K);
    let h = fold_mul(h ^ b, K);
    let h = fold_mul(h ^ c, K);
    fold_mul(h ^ d, K)
}

/// Latency storage in bytes. Blocks are large enough that the allocator maps each one separately (glibc's
/// mmap threshold starts at 128 KiB), so a block's untouched tail stays off RSS and dropping a block returns
/// its pages to the kernel at once.
const BLOCK: usize = 1 << 20;

/// Latencies are stored as 3-byte slots: a slot below ESCAPE is the latency itself; a latency of ESCAPE
/// (0xFF0000, 16.7 s) or more takes two slots, ESCAPE | its top byte, then its low 24 bits, so every value
/// in 0..=4294967295 (spec 2) stays exact. On seed 1 full 1,498 of 3.1M latencies take the two-slot form.
/// Measured against 4-byte words (a-10), this VM, seed 1 full: peak RssAnon 20.3 -> 16.0 MB at 1 worker,
/// 21.5 -> 17.3 at 4, 23.8 -> 19.2 at 12; time unchanged within noise.
const ESCAPE: u32 = 0xFF_0000;
const SLOT: usize = 3;

/// A chunk is `slots` 3-byte slots followed by a 4-byte link to the next chunk. A group's first chunk is small
/// because most (thread, group) pairs hold few latencies; later chunks hold 15 slots.
const FIRST_SLOTS: u32 = 3;
const SLOTS: u32 = 15;

/// One thread's latencies for one hour, kept apart per hour so the tail can drop them as soon as that
/// hour is merged. An address is block index * BLOCK + byte offset; chunks never straddle blocks.
struct Arena {
    hour: u32,
    blocks: Vec<Vec<u8>>,
    used: usize, // bytes used in the last block
}

impl Arena {
    fn alloc(&mut self, slots: u32) -> u32 {
        let len = slots as usize * SLOT + 4;
        if self.blocks.is_empty() || self.used + len > BLOCK {
            // vec! of zeros is calloc, which leaves fresh mapped pages untouched until written.
            self.blocks.push(vec![0; BLOCK]);
            self.used = 0;
        }
        let addr = (self.blocks.len() - 1) * BLOCK + self.used;
        self.used += len;
        addr as u32
    }

    fn bytes(&mut self, addr: u32, n: usize) -> &mut [u8] {
        let off = addr as usize % BLOCK;
        &mut self.blocks[addr as usize / BLOCK][off..off + n]
    }
}

/// One (hour, endpoint) group as one thread saw it: 16 bytes. Its key is where it sits: the hour's arena
/// map, at the endpoint's id; the free room in its current chunk follows from `slots`.
struct Group {
    slots: u32,  // 3-byte slots written, two for an escaped latency
    errors: u32,
    head: u32,   // address of the first chunk
    next: u32,   // address of the next free slot, or of the link when the chunk is full
}

impl Group {
    /// Writes one slot, chaining a new chunk when the current one is full.
    fn push_slot(&mut self, a: &mut Arena, v: u32) {
        let full = self.slots == FIRST_SLOTS || (self.slots > FIRST_SLOTS && (self.slots - FIRST_SLOTS) % SLOTS == 0);
        if full {
            let addr = a.alloc(SLOTS);
            a.bytes(self.next, 4).copy_from_slice(&addr.to_le_bytes());
            self.next = addr;
        }
        a.bytes(self.next, SLOT).copy_from_slice(&v.to_le_bytes()[..SLOT]);
        self.next += SLOT as u32;
        self.slots += 1;
    }
}

#[derive(Clone, Copy)]
struct Slot {
    tag: u32, // high hash bits, to skip most key compares
    id: u32,  // endpoint id; u32::MAX marks an empty slot
}

const EMPTY: Slot = Slot { tag: 0, id: u32::MAX };

/// Two-level key. An open-addressing (linear probing) table interns each endpoint to a local id; it holds
/// only the distinct endpoints, so it stays small and cache-resident. Each hour's arena then maps endpoint
/// id -> group with a plain array index, with no second probe.
/// Measured against one (hour, endpoint) table (a-6), this VM, seed 1 full: scan 90 -> 70 ns per line at
/// 1 worker, 94 -> 72 at 4; peak RssAnon -1.4 MB (1 worker) to -3 MB (12 workers).
struct Table {
    slots: Vec<Slot>,
    eps: Vec<(u32, u8)>, // endpoint id -> (offset in the mapping, length); spec 2: at most 125 bytes
    maps: Vec<Vec<u32>>, // per arena: endpoint id -> group, u32::MAX for none yet
    groups: Vec<Group>,
    arenas: Vec<Arena>,
}

impl Table {
    fn new() -> Self {
        Table { slots: vec![EMPTY; 1024], eps: Vec::new(), maps: Vec::new(), groups: Vec::new(), arenas: Vec::new() }
    }

    fn arena(&mut self, hour: u32) -> usize {
        match self.arenas.iter().position(|a| a.hour == hour) {
            Some(i) => i,
            None => {
                self.arenas.push(Arena { hour, blocks: Vec::new(), used: 0 });
                self.maps.push(Vec::new());
                self.arenas.len() - 1
            }
        }
    }

    fn ep_bytes(&self, data: &'static [u8], id: usize) -> &'static [u8] {
        let (off, len) = self.eps[id];
        &data[off as usize..off as usize + len as usize]
    }

    /// The local id of an endpoint, interning it on first sight.
    fn endpoint(&mut self, data: &'static [u8], ep: &'static [u8]) -> usize {
        let h = key_hash(0, ep);
        let mask = self.slots.len() - 1;
        let mut i = h as usize & mask;
        loop {
            let s = self.slots[i];
            if s.id == u32::MAX {
                let id = self.eps.len();
                self.slots[i] = Slot { tag: (h >> 32) as u32, id: id as u32 };
                let off = ep.as_ptr() as usize - data.as_ptr() as usize;
                self.eps.push((off as u32, u8::try_from(ep.len()).expect("endpoint longer than spec 2 allows")));
                if self.eps.len() * 2 > self.slots.len() {
                    self.grow(data);
                }
                return id;
            }
            if s.tag == (h >> 32) as u32 && self.ep_bytes(data, s.id as usize) == ep {
                return s.id as usize;
            }
            i = (i + 1) & mask;
        }
    }

    fn add(&mut self, data: &'static [u8], arena: usize, ep: &'static [u8], status: u32, latency: u32) {
        let e = self.endpoint(data, ep);
        let map = &mut self.maps[arena];
        if e >= map.len() {
            map.resize(self.eps.len(), u32::MAX);
        }
        let id = match map[e] {
            u32::MAX => {
                let id = self.groups.len();
                map[e] = id as u32;
                let head = self.arenas[arena].alloc(FIRST_SLOTS);
                self.groups.push(Group { slots: 0, errors: 0, head, next: head });
                id
            }
            id => id as usize,
        };
        let g = &mut self.groups[id];
        let a = &mut self.arenas[arena];
        if latency < ESCAPE {
            g.push_slot(a, latency);
        } else {
            g.push_slot(a, ESCAPE | latency >> 24);
            g.push_slot(a, latency & 0xFF_FFFF);
        }
        g.errors += (500..=599).contains(&status) as u32;
    }

    fn grow(&mut self, data: &'static [u8]) {
        let mut slots = vec![EMPTY; self.slots.len() * 2];
        let mask = slots.len() - 1;
        for s in self.slots.iter().filter(|s| s.id != u32::MAX) {
            let mut i = key_hash(0, self.ep_bytes(data, s.id as usize)) as usize & mask;
            while slots[i].id != u32::MAX {
                i = (i + 1) & mask;
            }
            slots[i] = *s;
        }
        self.slots = slots;
    }
}

/// Aggregates every line starting in data[start..end].
fn scan(data: &'static [u8], start: usize, end: usize) -> Table {
    let mut table = Table::new();
    let mut prev_prefix: &[u8] = &[];
    let mut arena = 0;
    let mut p = start;
    while p < end {
        // Lines usually share the previous line's hour, so compare the prefix before parsing it.
        let prefix = &data[p..p + HOUR_PREFIX];
        if prefix != prev_prefix {
            arena = table.arena(parse_hour(prefix));
            prev_prefix = prefix;
        }
        let (ep_start, lf) = line_fields(data, p);
        let sp = latency_space(data, lf);
        let latency = parse_u32(&data[sp + 1..lf]);
        // Spec 2 grammar: status is exactly three digits followed by SP.
        let st = &data[sp - 3..sp];
        let status = digit(st[0]) * 100 + digit(st[1]) * 10 + digit(st[2]);
        table.add(data, arena, &data[ep_start..sp - 4], status, latency);
        p = lf + 1;
    }
    table
}

fn push_u64(out: &mut Vec<u8>, mut v: u64) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    out.extend_from_slice(&buf[i..]);
}

/// Appends "YYYY-MM-DDTHH:00:00Z" for an hour encoded as YYYYMMDDHH.
fn push_hour(out: &mut Vec<u8>, hour: u32) {
    let two = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&[b'0' + (v / 10) as u8, b'0' + (v % 10) as u8]);
    let (year, mmddhh) = (hour / 1_000_000, hour % 1_000_000);
    two(out, year / 100);
    two(out, year % 100);
    out.push(b'-');
    two(out, mmddhh / 10_000);
    out.push(b'-');
    two(out, mmddhh / 100 % 100);
    out.push(b'T');
    two(out, mmddhh % 100);
    out.extend_from_slice(b":00:00Z");
}

/// Exact nearest-rank p50, p95, p99 of an unsorted slice by selection, which is linear where a full sort is not.
fn percentiles(lat: &mut [u32]) -> [u32; 3] {
    // Spec 3.4: 0-based index of rank ceil(p * n / 100).
    let idx = |p: usize| (p * lat.len() + 99) / 100 - 1;
    let (i50, i95, i99) = (idx(50), idx(95), idx(99));
    // Select the highest rank first: everything left of it is no larger, so each lower rank is selected
    // within the left part only.
    let (_, &mut v99, _) = lat.select_nth_unstable(i99);
    let (_, &mut v95, _) = lat[..=i99].select_nth_unstable(i95);
    let (_, &mut v50, _) = lat[..=i95].select_nth_unstable(i50);
    [v50, v95, v99]
}

/// What a scan thread hands to the tail: its groups, and its endpoints as (offset, length) in the mapping.
struct Scanned {
    groups: Vec<Group>,
    eps: Vec<(u32, u8)>,
}

/// An endpoint's first 16 bytes as a big-endian integer, zero-padded: integer order equals byte order on
/// those bytes, and a shorter endpoint (spec 2: no zero bytes) sorts first, as spec 4 requires.
fn prefix_key(ep: &[u8]) -> u128 {
    let mut b = [0u8; 16];
    let n = ep.len().min(16);
    b[..n].copy_from_slice(&ep[..n]);
    u128::from_be_bytes(b)
}

/// One global endpoint order: every thread's distinct endpoints merged by bytes (spec 4), giving the
/// sorted list of all endpoints and, per thread, local id -> global rank. Each thread's list is sorted in
/// parallel first; the merge compares 16-byte integer prefixes and falls back to bytes only on a tie.
/// It replaces the per-thread sort of every group by (hour, endpoint bytes) and the tail's byte sort.
/// Measured on this VM against the two-level key alone: under 0.1 ms for the merge; total 0.121 -> 0.115 s at
/// 4 workers, 0.123 -> 0.120 s at 12.
fn global_ranks(data: &'static [u8], scanned: &[Scanned], sorted: &[Vec<(u128, u32)>]) -> (Vec<&'static [u8]>, Vec<Vec<u32>>) {
    let ep = |t: usize, e: u32| {
        let (off, len) = scanned[t].eps[e as usize];
        &data[off as usize..off as usize + len as usize]
    };
    let key = |t: usize, (k, e): (u128, u32)| (k, ep(t, e));
    let mut ranks: Vec<Vec<u32>> = scanned.iter().map(|s| vec![0; s.eps.len()]).collect();
    let mut all: Vec<&'static [u8]> = Vec::new();
    let mut pos = vec![0usize; scanned.len()];
    loop {
        let mut best: Option<usize> = None;
        for t in 0..scanned.len() {
            if pos[t] < sorted[t].len() && best.is_none_or(|b| key(t, sorted[t][pos[t]]) < key(b, sorted[b][pos[b]])) {
                best = Some(t);
            }
        }
        let Some(b) = best else { return (all, ranks) };
        let e = ep(b, sorted[b][pos[b]].1);
        if all.last() != Some(&e) {
            all.push(e);
        }
        ranks[b][sorted[b][pos[b]].1 as usize] = all.len() as u32 - 1;
        pos[b] += 1;
    }
}

/// Appends a group's latencies by walking its chunk chain and decoding its slots.
fn read_latencies(blocks: &[Vec<u8>], g: &Group, out: &mut Vec<u32>) {
    let (mut left, mut addr, mut cap) = (g.slots as usize, g.head as usize, FIRST_SLOTS as usize);
    let mut top: Option<u32> = None; // the top byte of an escaped latency, waiting for its low slot
    loop {
        let block = &blocks[addr / BLOCK];
        let off = addr % BLOCK;
        for i in 0..left.min(cap) {
            let b = &block[off + i * SLOT..off + i * SLOT + SLOT];
            let v = u32::from_le_bytes([b[0], b[1], b[2], 0]);
            match top.take() {
                Some(t) => out.push(t << 24 | v),
                None if v >= ESCAPE => top = Some(v & 0xFF),
                None => out.push(v),
            }
        }
        if left <= cap {
            return;
        }
        left -= cap;
        let link = &block[off + cap * SLOT..off + cap * SLOT + 4];
        addr = u32::from_le_bytes(link.try_into().unwrap()) as usize;
        cap = SLOTS as usize;
    }
}

/// Merges every thread's groups of one hour and formats their output lines, in endpoint order (spec 4).
/// `parts[t]` is thread t's latency blocks and endpoint -> group map for this hour; groups are ordered by
/// global endpoint rank, an integer sort with no byte compares.
fn format_hour(scanned: &[Scanned], all: &[&[u8]], ranks: &[Vec<u32>], parts: &[(Vec<Vec<u8>>, Vec<u32>)], hour: u32, scratch: &mut Vec<u32>) -> Vec<u8> {
    let mut refs: Vec<(u32, usize, &Group)> = Vec::new();
    for (t, (_, map)) in parts.iter().enumerate() {
        for (e, &g) in map.iter().enumerate().filter(|&(_, &g)| g != u32::MAX) {
            refs.push((ranks[t][e], t, &scanned[t].groups[g as usize]));
        }
    }
    refs.sort_unstable_by_key(|r| r.0);
    let mut hour_text = Vec::with_capacity(20);
    push_hour(&mut hour_text, hour);
    let mut buf = Vec::new();
    for run in refs.chunk_by(|x, y| x.0 == y.0) {
        scratch.clear();
        let mut errors = 0u64;
        for &(_, t, g) in run {
            read_latencies(&parts[t].0, g, scratch);
            errors += g.errors as u64;
        }
        let count = scratch.len() as u64;
        let [p50, p95, p99] = percentiles(scratch);
        buf.extend_from_slice(&hour_text);
        buf.push(b' ');
        buf.extend_from_slice(all[run[0].0 as usize]);
        // Spec 3.6: error_ppm = floor(errors * 1000000 / count), in 64 bits.
        for v in [count, p50 as u64, p95 as u64, p99 as u64, errors, errors * 1_000_000 / count] {
            buf.push(b' ');
            push_u64(&mut buf, v);
        }
        buf.push(b'\n');
    }
    buf
}

fn main() {
    unsafe extern "C" {
        fn mallopt(param: i32, value: i32) -> i32;
    }
    // Pin glibc's mmap threshold (M_MMAP_THRESHOLD, -3) at its 128 KiB default. Left dynamic, freeing the first
    // latency block raises it to 1 MiB, so the tail's output buffers come from the heap and their freed pages
    // stay charged while the output file grows. Measured (this VM, 12 workers, peak of RssAnon + output size):
    // 22.4 -> 19.8 MB median.
    unsafe { mallopt(-3, 128 * 1024) };
    let file = File::open(INPUT).expect("open input");
    let len = file.metadata().expect("stat input").len() as usize;
    let mut out = File::create(OUTPUT).expect("create output");
    if len == 0 {
        // Spec 3.7: an empty log gives an empty summary.
        return;
    }
    // Endpoints are kept as u32 offsets into the mapping; spec 2's lines are at most 176 bytes, so 3.1M lines stay far below 4 GiB.
    assert!(len <= u32::MAX as usize, "input over 4 GiB");
    let data = map_file(&file, len);

    let workers = worker_count();
    let ranges = line_ranges(data, workers);
    let mut per_thread: Vec<(Scanned, Vec<(u128, u32)>, Vec<Arena>, Vec<Vec<u32>>)> = thread::scope(|s| {
        let handles: Vec<_> = ranges
            .iter()
            .map(|&(a, b)| {
                s.spawn(move || {
                    let Table { groups, eps, maps, arenas, .. } = scan(data, a, b);
                    release_range(data, a, b);
                    // This thread's endpoint ids in byte order, sorted here in parallel for the global merge.
                    let bytes = |&(off, len): &(u32, u8)| &data[off as usize..off as usize + len as usize];
                    let mut sorted: Vec<(u128, u32)> = (0..eps.len() as u32).map(|e| (prefix_key(bytes(&eps[e as usize])), e)).collect();
                    sorted.sort_unstable_by_key(|&(k, e)| (k, bytes(&eps[e as usize])));
                    (Scanned { groups, eps }, sorted, arenas, maps)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let sorted: Vec<Vec<(u128, u32)>> = per_thread.iter_mut().map(|p| std::mem::take(&mut p.1)).collect();
    let mut arenas: Vec<(Vec<Arena>, Vec<Vec<u32>>)> =
        per_thread.iter_mut().map(|p| (std::mem::take(&mut p.2), std::mem::take(&mut p.3))).collect();
    let scanned: Vec<Scanned> = per_thread.into_iter().map(|p| p.0).collect();
    let (all, ranks) = global_ranks(data, &scanned, &sorted);
    drop(sorted);

    // The tail is split by hour: hours are disjoint parts of the key space that are also contiguous in the
    // output, so each part is merged, selected and formatted by one thread with no shared writes, and the
    // parts are written in order. The hours come from the input itself.
    let mut hours: Vec<u32> = arenas.iter().flat_map(|(a, _)| a.iter().map(|a| a.hour)).collect();
    hours.sort_unstable();
    hours.dedup();
    // Each hour's job owns every thread's latency blocks and endpoint map for that hour, so they are dropped
    // as soon as the hour is formatted. Each mutex is taken exactly once, by the thread that claims the hour:
    // never contended.
    let mut jobs: Vec<Vec<(Vec<Vec<u8>>, Vec<u32>)>> =
        (0..hours.len()).map(|_| (0..workers).map(|_| (Vec::new(), Vec::new())).collect()).collect();
    for (t, (thread_arenas, maps)) in arenas.iter_mut().enumerate() {
        for (a, map) in thread_arenas.iter_mut().zip(maps.iter_mut()) {
            jobs[hours.binary_search(&a.hour).unwrap()][t] = (std::mem::take(&mut a.blocks), std::mem::take(map));
        }
    }
    drop(arenas);
    let jobs: Vec<Mutex<Vec<(Vec<Vec<u8>>, Vec<u32>)>>> = jobs.into_iter().map(Mutex::new).collect();

    // Threads take the next hour from a shared counter, so uneven hours still balance.
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel::<(usize, Vec<u8>)>();
    thread::scope(|s| {
        for _ in 0..workers {
            let (scanned, all, ranks, jobs, hours, next, tx) = (&scanned, &all, &ranks, &jobs, &hours, &next, tx.clone());
            s.spawn(move || {
                let mut scratch = Vec::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= hours.len() {
                        break;
                    }
                    let parts = std::mem::take(&mut *jobs[i].lock().unwrap());
                    let buf = format_hour(scanned, all, ranks, &parts, hours[i], &mut scratch);
                    drop(parts);
                    tx.send((i, buf)).unwrap();
                }
            });
        }
        drop(tx);
        // Write each hour's buffer as soon as every earlier hour is written, overlapping the write with the
        // remaining work and freeing buffers early.
        let mut pending: Vec<Option<Vec<u8>>> = vec![None; hours.len()];
        let mut written = 0;
        for (i, buf) in rx {
            pending[i] = Some(buf);
            while let Some(buf) = pending.get_mut(written).and_then(Option::take) {
                out.write_all(&buf).expect("write output");
                written += 1;
            }
        }
    });
}
