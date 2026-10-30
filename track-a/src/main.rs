//! Track A: per (hour, endpoint) request count, nearest-rank p50/p95/p99 latency and 5xx rate.
//! One pass over an mmap of /data/requests.log, split into byte ranges, one private table per thread;
//! the merge, percentile selection and formatting then run in parallel, one hour per task.

use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::os::fd::AsRawFd;
use std::thread;

const INPUT: &str = "/data/requests.log";
const OUTPUT: &str = "/out/summary.txt";

// Spec 2 grammar: ts is always 27 bytes, then SP, so the method starts at byte 28.
const METHOD_START: usize = 28;
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

struct Group {
    hour: u32,
    ep: &'static [u8],
    latencies: Vec<u32>,
    errors: u64,
}

#[derive(Clone, Copy)]
struct Slot {
    hash: u64,
    group: u32, // u32::MAX marks an empty slot
}

const EMPTY: Slot = Slot { hash: 0, group: u32::MAX };

/// Open-addressing (linear probing) table from (hour, endpoint) to a group; grows at half load.
struct Table {
    slots: Vec<Slot>,
    groups: Vec<Group>,
}

impl Table {
    fn new() -> Self {
        Table { slots: vec![EMPTY; 1024], groups: Vec::new() }
    }

    fn group(&mut self, hour: u32, ep: &'static [u8]) -> &mut Group {
        let h = key_hash(hour, ep);
        let mask = self.slots.len() - 1;
        let mut i = (h >> 32) as usize & mask;
        loop {
            let s = self.slots[i];
            if s.group == u32::MAX {
                break;
            }
            if s.hash == h {
                let g = &self.groups[s.group as usize];
                if g.hour == hour && g.ep == ep {
                    return &mut self.groups[s.group as usize];
                }
            }
            i = (i + 1) & mask;
        }
        let id = self.groups.len();
        self.slots[i] = Slot { hash: h, group: id as u32 };
        self.groups.push(Group { hour, ep, latencies: Vec::new(), errors: 0 });
        if self.groups.len() * 2 > self.slots.len() {
            self.grow();
        }
        &mut self.groups[id]
    }

    fn grow(&mut self) {
        let mut slots = vec![EMPTY; self.slots.len() * 2];
        let mask = slots.len() - 1;
        for s in self.slots.iter().filter(|s| s.group != u32::MAX) {
            let mut i = (s.hash >> 32) as usize & mask;
            while slots[i].group != u32::MAX {
                i = (i + 1) & mask;
            }
            slots[i] = *s;
        }
        self.slots = slots;
    }
}

/// Aggregates every line starting in data[start..end].
fn scan(data: &'static [u8], start: usize, end: usize) -> Vec<Group> {
    let mut table = Table::new();
    let mut prev_prefix: &[u8] = &[];
    let mut hour = 0u32;
    let mut p = start;
    while p < end {
        // Lines usually share the previous line's hour, so compare the prefix before parsing it.
        let prefix = &data[p..p + HOUR_PREFIX];
        if prefix != prev_prefix {
            hour = parse_hour(prefix);
            prev_prefix = prefix;
        }
        // The method never contains '/', so the first '/' after it opens the endpoint.
        let ep_start = p + METHOD_START + data[p + METHOD_START..].iter().position(|&b| b == b'/').expect("no endpoint");
        let lf = ep_start + data[ep_start..].iter().position(|&b| b == b'\n').unwrap_or(data.len() - ep_start);
        // Latency is the last field, so walk back from the line end to its space.
        let mut sp = lf - 1;
        while data[sp] != b' ' {
            sp -= 1;
        }
        let latency = parse_u32(&data[sp + 1..lf]);
        // Spec 2 grammar: status is exactly three digits followed by SP.
        let st = &data[sp - 3..sp];
        let status = digit(st[0]) * 100 + digit(st[1]) * 10 + digit(st[2]);
        let ep = &data[ep_start..sp - 4];
        let g = table.group(hour, ep);
        g.latencies.push(latency);
        g.errors += (500..=599).contains(&status) as u64;
        p = lf + 1;
    }
    table.groups
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

/// Merges every thread's groups of one hour and formats their output lines, in endpoint order (spec 4).
/// Each thread's groups are sorted by (hour, endpoint), so the hour is one contiguous run in each of them.
fn format_hour(tables: &[Vec<Group>], hour: u32, scratch: &mut Vec<u32>) -> Vec<u8> {
    let mut refs: Vec<&Group> = tables
        .iter()
        .flat_map(|gs| &gs[gs.partition_point(|g| g.hour < hour)..gs.partition_point(|g| g.hour <= hour)])
        .collect();
    refs.sort_unstable_by(|x, y| x.ep.cmp(y.ep));
    let mut hour_text = Vec::with_capacity(20);
    push_hour(&mut hour_text, hour);
    let mut buf = Vec::new();
    for run in refs.chunk_by(|x, y| x.ep == y.ep) {
        scratch.clear();
        let mut errors = 0u64;
        for g in run {
            scratch.extend_from_slice(&g.latencies);
            errors += g.errors;
        }
        let count = scratch.len() as u64;
        let [p50, p95, p99] = percentiles(scratch);
        buf.extend_from_slice(&hour_text);
        buf.push(b' ');
        buf.extend_from_slice(run[0].ep);
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
    let data = map_file(&file, len);

    let workers = worker_count();
    let ranges = line_ranges(data, workers);
    let tables: Vec<Vec<Group>> = thread::scope(|s| {
        let handles: Vec<_> = ranges
            .iter()
            .map(|&(a, b)| {
                s.spawn(move || {
                    // Sorted here, in parallel, so the tail finds each hour as one run per thread.
                    let mut groups = scan(data, a, b);
                    groups.sort_unstable_by(|x, y| (x.hour, x.ep).cmp(&(y.hour, y.ep)));
                    groups
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // The tail is split by hour: hours are disjoint parts of the key space that are also contiguous in the
    // output, so each part is merged, selected and formatted by one thread with no locks, and the parts are
    // written in order. The hours come from the input itself.
    let mut hours: Vec<u32> =
        tables.iter().flat_map(|gs| gs.chunk_by(|x, y| x.hour == y.hour).map(|run| run[0].hour)).collect();
    hours.sort_unstable();
    hours.dedup();

    // Threads take the next hour from a shared counter, so uneven hours still balance.
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel::<(usize, Vec<u8>)>();
    thread::scope(|s| {
        for _ in 0..workers {
            let (tables, hours, next, tx) = (&tables, &hours, &next, tx.clone());
            s.spawn(move || {
                let mut scratch = Vec::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= hours.len() {
                        break;
                    }
                    tx.send((i, format_hour(tables, hours[i], &mut scratch))).unwrap();
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
