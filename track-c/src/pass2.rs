//! The parallel pass: workers claim chunks of records.csv from a shared counter, parse each into
//! their own per-group slots, and send each record's name to its owner (names.rs). A worker writes
//! its messages into one bucket per owner through raw cursors, hands a full bucket to the owner's
//! inbox, and files the buckets in its own inbox at each of its hand-overs and after every chunk.
//! No rounds and no barriers
//! (8a60e4e's routing: a slow CPU delays only its own chunks and inbox, c-18 probe w13).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::names::{Msg, Name, Owner, owner};
use crate::{records::Slot, records::parse_chunk, records::record_start, regions::Build};

unsafe extern "C" {
    fn madvise(addr: *const u8, len: usize, advice: i32) -> i32;
}

/// Bytes per chunk: a design constant, about 800 chunks for the 202 MB file, so the last chunks to
/// finish are short whoever holds them.
pub const CHUNK: usize = 1 << 18;

/// Slots per bucket (4 KiB): a full bucket is one hand-off to its owner.
const BUCKET: usize = 256;

/// Full buckets an owner's mailbox may hold before a sender files them itself. A stalled owner (a
/// descheduled or late thread) let 11 senders run ahead: mailboxes of 300-380 buckets and 2,300-3,900
/// fresh 4 KiB buckets per run (12-15 MB, laptop counts, c-52 s2). Never reached while owners keep up.
const LAG: usize = 16;

/// An owner's mailbox: the full buckets sent to it, and the empty ones filed, which senders take back.
#[derive(Default)]
struct Mailbox {
    full: Vec<Vec<Msg>>,
    empty: Vec<Vec<Msg>>,
}

/// What the pass returns.
pub struct Pass2 {
    /// Per worker, its per-group slots.
    pub slots: Vec<Vec<Slot>>,
    /// names_distinct per group.
    pub distinct: Vec<u64>,
    /// False when some chunk did not stop exactly on the next chunk's start (e1), i.e. a cut was not
    /// a record start; the caller must then rerun on one worker with one chunk.
    pub exact: bool,
}

/// One worker's sending side: its outgoing buckets, one per owner, written through raw cursors (no
/// Vec length or capacity work per message, c-49 b2: 22.47 ms of pass against 23.54 with Vec pushes).
struct Worker<'a> {
    w: usize,
    b: &'a [u8],
    boxes: &'a [Mutex<Mailbox>],
    /// Each owner's names; its own worker files them, a sender only to clear a backlog (LAG).
    owners: &'a [Mutex<Owner>],
    out: Vec<Vec<Msg>>,
    cur: Vec<*mut Msg>,
    /// Where each bucket is handed over: one slot short of BUCKET, so a two-slot name always fits.
    lim: Vec<*mut Msg>,
}

impl<'a> Worker<'a> {
    fn new(w: usize, b: &'a [u8], boxes: &'a [Mutex<Mailbox>], owners: &'a [Mutex<Owner>]) -> Self {
        let mut out: Vec<Vec<Msg>> = (0..boxes.len()).map(|_| Vec::with_capacity(BUCKET)).collect();
        let cur: Vec<*mut Msg> = out.iter_mut().map(|v| v.as_mut_ptr()).collect();
        // SAFETY: each bucket has room for BUCKET slots.
        let lim = cur.iter().map(|&c| unsafe { c.add(BUCKET - 1) }).collect();
        Worker { w, b, boxes, owners, out, cur, lim }
    }

    /// Files every bucket in owner `o`'s mailbox and returns them emptied, unless another worker is
    /// filing for `o` right now (`wait`: then waits for it). Never inlined: it runs once per bucket or
    /// chunk, and profiles then show the owner's work apart.
    #[inline(never)]
    fn drain(&self, o: usize, wait: bool) {
        let mut own = match self.owners[o].try_lock() {
            Ok(own) => own,
            Err(_) if wait => self.owners[o].lock().unwrap(),
            Err(_) => return,
        };
        let mut buckets = std::mem::take(&mut self.boxes[o].lock().unwrap().full);
        if buckets.is_empty() {
            return;
        }
        for bucket in &mut buckets {
            own.file(bucket, self.b);
            bucket.clear();
        }
        self.boxes[o].lock().unwrap().empty.append(&mut buckets);
    }

    /// Writes a record's name into its owner's bucket.
    #[inline(always)]
    fn send(&mut self, name: Name) {
        let workers = self.out.len();
        // SAFETY: owner < workers; its cursor is below its limit (hand_over runs once it gets there),
        // so both slots land inside the bucket's capacity.
        unsafe {
            let (o, c) = match name {
                Name::One(h, m) => {
                    let o = owner(h, workers);
                    let c = *self.cur.get_unchecked(o);
                    c.write(m);
                    (o, c.add(1))
                }
                Name::Two(h, m, tail) => {
                    let o = owner(h, workers);
                    let c = *self.cur.get_unchecked(o);
                    c.write(m);
                    c.add(1).write(tail);
                    (o, c.add(2))
                }
            };
            *self.cur.get_unchecked_mut(o) = c;
            if c >= *self.lim.get_unchecked(o) {
                self.hand_over(o);
            }
        }
    }

    /// Hands owner `o`'s bucket to its mailbox and takes an empty one back, then files this worker's
    /// own mailbox (and `o`'s past LAG). The rare path, kept out of the parse loop (c-49). Filing at
    /// each hand-over, not only after each chunk, keeps the buckets few and their slots still cached.
    #[cold]
    #[inline(never)]
    fn hand_over(&mut self, o: usize) {
        let full = self.seal(o);
        let (fresh, backlog) = {
            let mut mailbox = self.boxes[o].lock().unwrap();
            mailbox.full.push(full);
            (mailbox.empty.pop(), mailbox.full.len())
        };
        if backlog > LAG {
            self.drain(o, false);
        }
        self.out[o] = fresh.unwrap_or_else(|| Vec::with_capacity(BUCKET));
        self.cur[o] = self.out[o].as_mut_ptr();
        // SAFETY: as in `new`.
        self.lim[o] = unsafe { self.cur[o].add(BUCKET - 1) };
        self.drain(self.w, false);
    }

    /// Owner `o`'s bucket with its length set to the slots written, leaving an empty Vec behind.
    fn seal(&mut self, o: usize) -> Vec<Msg> {
        let mut v = std::mem::take(&mut self.out[o]);
        // SAFETY: every slot from the bucket's start up to cur[o] was written, inside its capacity.
        unsafe { v.set_len(self.cur[o].offset_from(v.as_ptr()) as usize) };
        v
    }
}

/// Runs the pass on `workers` threads over the records of `b[..len]` from `first` to the end (`b`
/// holds the guard after them), cut into `chunk`-byte pieces at record starts. Every worker first
/// helps build the region table (`Build::ready`; at once on a rerun).
pub fn run(b: &[u8], len: usize, first: usize, chunk: usize, workers: usize, regions: &Build) -> Pass2 {
    let file = &b[..len];
    let chunks = (len - first).div_ceil(chunk);
    let start = |k: usize| if k == 0 { first } else { record_start(file, (first + k * chunk).min(len)) };
    let boxes: Vec<Mutex<Mailbox>> = (0..workers).map(|_| Mutex::default()).collect();
    let owners: Vec<Mutex<Owner>> = (0..workers).map(|_| Mutex::new(Owner::new())).collect();
    let next = AtomicUsize::new(0);
    let flushed = AtomicUsize::new(0);
    let exact = AtomicBool::new(true);

    let worker = |w: usize| {
        let (lookup, groups) = regions.ready();
        let mut slots = vec![Slot::new(); groups];
        let mut me = Worker::new(w, b, &boxes, &owners);
        loop {
            let k = next.fetch_add(1, Ordering::Relaxed);
            if k >= chunks {
                break;
            }
            let end = start(k + 1);
            let on_record = |g: usize, at: usize, n: usize| {
                // SAFETY: parse_chunk passes a name span inside the padded mapping, after the quote
                // or comma that opens it; GUARD covers 32 bytes past its start.
                me.send(unsafe { Name::new(b.as_ptr(), at, n, g) })
            };
            let stop = parse_chunk(b, len, start(k), end, lookup, &mut slots, on_record);
            // Pairwise end check (e1): the parse must stop exactly on the next chunk's start.
            exact.fetch_and(stop == end, Ordering::Relaxed);
            me.drain(w, false);
        }
        // The mapping's page-table entries (about 50k for the 202 MB file) were torn down at exit on
        // one CPU; each worker drops its share now (c-37f). A page touched again (a neighbour's last
        // record, a rerun, an exact name's compare) faults back in from the page cache. The last
        // partial page is kept: it holds the guard (main.rs), a private copy MADV_DONTNEED would discard.
        let pages = len & !4095;
        let share = pages.div_ceil(workers).next_multiple_of(4096);
        let from = (w * share).min(pages);
        // SAFETY: a page-aligned range inside the mapping; madvise only drops page-table entries.
        unsafe { madvise(b.as_ptr().add(from), share.min(pages - from), 4) }; // 4 = MADV_DONTNEED
        for o in 0..workers {
            let bucket = me.seal(o);
            boxes[o].lock().unwrap().full.push(bucket);
        }
        // Once every worker has flushed (each flush is under the mailbox locks, before its count), no
        // bucket can arrive any more: one last drain, waiting out a sender filing here, empties this
        // mailbox for good.
        flushed.fetch_add(1, Ordering::Release);
        while flushed.load(Ordering::Acquire) < workers {
            me.drain(w, false);
            std::thread::yield_now();
        }
        me.drain(w, true);
        slots
    };
    // The workers start before the region table exists and build it first (c-47 s2); this thread is
    // worker 0, so it spawns one thread fewer and joins in once the others are spawned.
    let slots: Vec<Vec<Slot>> = std::thread::scope(|s| {
        let handles: Vec<_> = (1..workers).map(|w| s.spawn(move || worker(w))).collect();
        let mine = worker(0);
        std::iter::once(mine).chain(handles.into_iter().map(|h| h.join().unwrap())).collect()
    });
    let counts: Vec<[u64; 256]> = owners.into_iter().map(|own| own.into_inner().unwrap().counts).collect();
    let distinct = (0..slots[0].len()).map(|g| counts.iter().map(|c| c[g]).sum()).collect();
    Pass2 { slots, distinct, exact: exact.load(Ordering::Relaxed) }
}
