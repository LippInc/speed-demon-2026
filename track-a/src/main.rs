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

/// Latency storage in 4-byte words. Blocks are large enough that the allocator maps each one separately
/// (glibc's mmap threshold starts at 128 KiB), so a block's untouched tail stays off RSS and dropping a block
/// returns its pages to the kernel at once.
const BLOCK: usize = 1 << 18;

/// A chunk is `len - 1` latencies followed by one link word holding the next chunk's address. A group's
/// first chunk is small because most (thread, group) pairs hold few latencies; later chunks are 16 words,
/// so a large group wastes under 16 words plus one link per 15 latencies.
const FIRST_CHUNK: usize = 4;
const CHUNK: usize = 16;

/// One thread's latencies for one hour, kept apart per hour so the tail can drop them as soon as that
/// hour is merged. An address is block index * BLOCK + offset; chunks never straddle blocks.
/// Measured against per-group Vecs (a-3), this VM, 12 workers: peak RssAnon 39.9 -> 26.7 MB, same time.
struct Arena {
    hour: u32,
    blocks: Vec<Vec<u32>>,
    used: usize, // words used in the last block
}

impl Arena {
    fn alloc(&mut self, len: usize) -> u32 {
        if self.blocks.is_empty() || self.used + len > BLOCK {
            // vec! of zeros is calloc, which leaves fresh mapped pages untouched until written.
            self.blocks.push(vec![0; BLOCK]);
            self.used = 0;
        }
        let addr = (self.blocks.len() - 1) * BLOCK + self.used;
        self.used += len;
        addr as u32
    }

    fn word(&mut self, addr: u32) -> &mut u32 {
        &mut self.blocks[addr as usize / BLOCK][addr as usize % BLOCK]
    }
}

/// One (hour, endpoint) group as one thread saw it: 24 bytes, against 56 for a key slice plus a Vec.
struct Group {
    ep_off: u32,
    ep_len: u8,        // spec 2: an endpoint is 1 to 125 bytes
    chunk_left: u8,    // free latency words in the current chunk
    arena: u16,        // index of the hour's arena in this thread; spec 2: the span is 72 hours
    count: u32,
    errors: u32,
    head: u32,         // address of the first chunk
    next: u32,         // address of the next free latency word, or of the link word when the chunk is full
}

impl Group {
    fn ep(&self, data: &'static [u8]) -> &'static [u8] {
        &data[self.ep_off as usize..self.ep_off as usize + self.ep_len as usize]
    }
}

#[derive(Clone, Copy)]
struct Slot {
    tag: u32, // high hash bits, to skip most key compares
    group: u32, // u32::MAX marks an empty slot
}

const EMPTY: Slot = Slot { tag: 0, group: u32::MAX };

/// Open-addressing (linear probing) table from (hour, endpoint) to a group; grows at half load.
struct Table {
    slots: Vec<Slot>,
    groups: Vec<Group>,
    arenas: Vec<Arena>,
}

impl Table {
    fn new() -> Self {
        Table { slots: vec![EMPTY; 1024], groups: Vec::new(), arenas: Vec::new() }
    }

    fn arena(&mut self, hour: u32) -> usize {
        match self.arenas.iter().position(|a| a.hour == hour) {
            Some(i) => i,
            None => {
                self.arenas.push(Arena { hour, blocks: Vec::new(), used: 0 });
                self.arenas.len() - 1
            }
        }
    }

    fn add(&mut self, data: &'static [u8], arena: usize, ep: &'static [u8], status: u32, latency: u32) {
        let h = key_hash(self.arenas[arena].hour, ep);
        let mask = self.slots.len() - 1;
        let mut i = h as usize & mask;
        let id = loop {
            let s = self.slots[i];
            if s.group == u32::MAX {
                let id = self.groups.len();
                self.slots[i] = Slot { tag: (h >> 32) as u32, group: id as u32 };
                let head = self.arenas[arena].alloc(FIRST_CHUNK);
                let ep_off = ep.as_ptr() as usize - data.as_ptr() as usize;
                self.groups.push(Group {
                    ep_off: ep_off as u32,
                    ep_len: u8::try_from(ep.len()).expect("endpoint longer than spec 2 allows"),
                    chunk_left: FIRST_CHUNK as u8 - 1,
                    arena: arena as u16,
                    count: 0,
                    errors: 0,
                    head,
                    next: head,
                });
                if self.groups.len() * 2 > self.slots.len() {
                    self.grow(data);
                }
                break id;
            }
            if s.tag == (h >> 32) as u32 {
                let g = &self.groups[s.group as usize];
                if g.arena as usize == arena && g.ep(data) == ep {
                    break s.group as usize;
                }
            }
            i = (i + 1) & mask;
        };
        let g = &mut self.groups[id];
        let a = &mut self.arenas[arena];
        if g.chunk_left == 0 {
            // The chunk is full and `next` is its link word: chain a new, larger chunk.
            let addr = a.alloc(CHUNK);
            *a.word(g.next) = addr;
            g.next = addr;
            g.chunk_left = CHUNK as u8 - 1;
        }
        *a.word(g.next) = latency;
        g.next += 1;
        g.chunk_left -= 1;
        g.count += 1;
        g.errors += (500..=599).contains(&status) as u32;
    }

    fn grow(&mut self, data: &'static [u8]) {
        let mut slots = vec![EMPTY; self.slots.len() * 2];
        let mask = slots.len() - 1;
        for s in self.slots.iter().filter(|s| s.group != u32::MAX) {
            let g = &self.groups[s.group as usize];
            let mut i = key_hash(self.arenas[g.arena as usize].hour, g.ep(data)) as usize & mask;
            while slots[i].group != u32::MAX {
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

/// What a scan thread hands to the tail: its groups sorted by (hour, endpoint) and each arena's hour.
struct Scanned {
    groups: Vec<Group>,
    arena_hours: Vec<u32>,
}

/// Appends a group's latencies by walking its chunk chain.
fn read_latencies(blocks: &[Vec<u32>], g: &Group, out: &mut Vec<u32>) {
    let (mut left, mut addr, mut cap) = (g.count as usize, g.head as usize, FIRST_CHUNK - 1);
    loop {
        let n = left.min(cap);
        let block = &blocks[addr / BLOCK];
        out.extend_from_slice(&block[addr % BLOCK..addr % BLOCK + n]);
        left -= n;
        if left == 0 {
            return;
        }
        addr = block[addr % BLOCK + cap] as usize;
        cap = CHUNK - 1;
    }
}

/// Merges every thread's groups of one hour and formats their output lines, in endpoint order (spec 4).
/// Each thread's groups are sorted by (hour, endpoint), so the hour is one contiguous run in each of them;
/// `blocks[t]` holds thread t's latencies for this hour.
fn format_hour(data: &'static [u8], scanned: &[Scanned], blocks: &[Vec<Vec<u32>>], hour: u32, scratch: &mut Vec<u32>) -> Vec<u8> {
    let mut refs: Vec<(usize, &Group)> = Vec::new();
    for (t, sc) in scanned.iter().enumerate() {
        let hour_of = |g: &Group| sc.arena_hours[g.arena as usize];
        let run = &sc.groups[sc.groups.partition_point(|g| hour_of(g) < hour)..sc.groups.partition_point(|g| hour_of(g) <= hour)];
        refs.extend(run.iter().map(|g| (t, g)));
    }
    refs.sort_unstable_by(|x, y| x.1.ep(data).cmp(y.1.ep(data)));
    let mut hour_text = Vec::with_capacity(20);
    push_hour(&mut hour_text, hour);
    let mut buf = Vec::new();
    for run in refs.chunk_by(|x, y| x.1.ep(data) == y.1.ep(data)) {
        scratch.clear();
        let mut errors = 0u64;
        for &(t, g) in run {
            read_latencies(&blocks[t], g, scratch);
            errors += g.errors as u64;
        }
        let count = scratch.len() as u64;
        let [p50, p95, p99] = percentiles(scratch);
        buf.extend_from_slice(&hour_text);
        buf.push(b' ');
        buf.extend_from_slice(run[0].1.ep(data));
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
    let file = File::open(INPUT).expect("open input");
    let len = file.metadata().expect("stat input").len() as usize;
    let mut out = File::create(OUTPUT).expect("create output");
    if len == 0 {
        // Spec 3.7: an empty log gives an empty summary.
        return;
    }
    // Groups keep endpoint offsets as u32; spec 2's lines are at most 176 bytes, so 3.1M lines stay far below 4 GiB.
    assert!(len <= u32::MAX as usize, "input over 4 GiB");
    let data = map_file(&file, len);

    let workers = worker_count();
    let ranges = line_ranges(data, workers);
    let (scanned, mut arenas): (Vec<Scanned>, Vec<Vec<Arena>>) = thread::scope(|s| {
        let handles: Vec<_> = ranges
            .iter()
            .map(|&(a, b)| {
                s.spawn(move || {
                    let Table { mut groups, arenas, .. } = scan(data, a, b);
                    let arena_hours: Vec<u32> = arenas.iter().map(|a| a.hour).collect();
                    // Sorted here, in parallel, so the tail finds each hour as one run per thread.
                    groups.sort_unstable_by(|x, y| {
                        (arena_hours[x.arena as usize], x.ep(data)).cmp(&(arena_hours[y.arena as usize], y.ep(data)))
                    });
                    (Scanned { groups, arena_hours }, arenas)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).unzip()
    });

    // The tail is split by hour: hours are disjoint parts of the key space that are also contiguous in the
    // output, so each part is merged, selected and formatted by one thread with no shared writes, and the
    // parts are written in order. The hours come from the input itself.
    let mut hours: Vec<u32> = arenas.iter().flatten().map(|a| a.hour).collect();
    hours.sort_unstable();
    hours.dedup();
    // Each hour's job owns every thread's latency blocks for that hour, so they are dropped as soon as the
    // hour is formatted. Each mutex is taken exactly once, by the thread that claims the hour: never contended.
    let mut jobs: Vec<Vec<Vec<Vec<u32>>>> = (0..hours.len()).map(|_| (0..workers).map(|_| Vec::new()).collect()).collect();
    for (t, thread_arenas) in arenas.iter_mut().enumerate() {
        for a in thread_arenas {
            jobs[hours.binary_search(&a.hour).unwrap()][t] = std::mem::take(&mut a.blocks);
        }
    }
    drop(arenas);
    let jobs: Vec<Mutex<Vec<Vec<Vec<u32>>>>> = jobs.into_iter().map(Mutex::new).collect();

    // Threads take the next hour from a shared counter, so uneven hours still balance.
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel::<(usize, Vec<u8>)>();
    thread::scope(|s| {
        for _ in 0..workers {
            let (scanned, jobs, hours, next, tx) = (&scanned, &jobs, &hours, &next, tx.clone());
            s.spawn(move || {
                let mut scratch = Vec::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= hours.len() {
                        break;
                    }
                    let blocks = std::mem::take(&mut *jobs[i].lock().unwrap());
                    let buf = format_hour(data, scanned, &blocks, hours[i], &mut scratch);
                    drop(blocks);
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
