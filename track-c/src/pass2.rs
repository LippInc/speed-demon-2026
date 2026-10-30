//! Pass 2: every worker parses its own records into its own per-group slots and interns each name
//! in the shared table. Each group's name bitset belongs to one worker (group % workers), which
//! alone writes it, in bounded rounds: a worker files each (group, id) pair into one bucket per
//! owner until any bucket is full, all workers meet at a barrier, each sets the bits of the groups
//! it owns from the buckets addressed to it, they meet again, and the next round starts with empty
//! buckets. Name memory is the intern table, the bitsets and workers x ROUND_PAIRS pairs, never a
//! function of the record count.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Barrier, RwLock};
use std::time::{Duration, Instant};

use crate::names::{Intern, NameSet};
use crate::records::{Slot, parse_range};
use crate::regions::Lookup;

/// Pairs one worker buffers per round (8 B each, so 128 KiB a worker), split evenly over its
/// per-owner buckets.
const ROUND_PAIRS: usize = 16 * 1024;

/// What pass 2 returns.
pub struct Pass2 {
    /// Per worker, its per-group slots.
    pub slots: Vec<Vec<Slot>>,
    /// names_distinct per group.
    pub distinct: Vec<u64>,
    pub records: u64,
    /// Per worker, the time until its range was parsed.
    pub times: Vec<Duration>,
    pub rounds: usize,
    /// False when some worker did not stop exactly on its cut, i.e. some start was not a record
    /// start (e1); the caller must then rerun on safe cuts.
    pub exact: bool,
}

/// One worker's result.
struct Worker {
    slots: Vec<Slot>,
    owned: Vec<NameSet>,
    records: u64,
    time: Duration,
    rounds: usize,
}

/// Runs pass 2 on one worker per `starts` window: worker k parses the records starting in
/// `starts[k]..starts[k + 1]`.
pub fn run<L: Lookup + ?Sized + Sync>(b: &[u8], starts: &[usize], lookup: &L, groups: usize, intern: &Intern) -> Pass2 {
    let workers = starts.len() - 1;
    // Group -> (owner, index among the owner's groups), so the hot loop needs no `%` or division.
    let route: Vec<(usize, u32)> = (0..groups).map(|g| (g % workers, (g / workers) as u32)).collect();
    // Per worker, one bucket per owner of (owner-local group, id). Each owner reads only the buckets
    // addressed to it, short contiguous runs, where it used to filter every worker's whole buffer:
    // the noowner probe (7632abe) put that step at about 24 ms of the box's T. Measured (this VM, 12
    // workers, full seed 1, interleaved medians of 7): pass2+names 325 -> 273 ms; 27 rounds instead of
    // 18, since skew fills the busiest owner's bucket first; M unchanged (sdrig, about 14.5 MB a run).
    let cap = (ROUND_PAIRS / workers).max(1);
    let buckets: Vec<RwLock<Vec<Vec<(u32, u32)>>>> =
        (0..workers).map(|_| RwLock::new((0..workers).map(|_| Vec::with_capacity(cap)).collect())).collect();
    let barrier = Barrier::new(workers);
    let finished = AtomicUsize::new(0);
    let exact = AtomicBool::new(true);

    let results: Vec<Worker> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|w| {
                let (buckets, route, barrier, finished, exact) =
                    (&buckets, &route, &barrier, &finished, &exact);
                s.spawn(move || {
                    let t = Instant::now();
                    let mut me = Worker {
                        slots: vec![Slot::default(); groups],
                        owned: (w..groups).step_by(workers).map(|_| NameSet::default()).collect(),
                        records: 0,
                        time: Duration::ZERO,
                        rounds: 0,
                    };
                    let (mut i, end) = (starts[w], starts[w + 1]);
                    let mut done = false;
                    loop {
                        // Between the previous round's second barrier and this round's first, only
                        // the writer touches its buckets.
                        {
                            let mut mine = buckets[w].write().unwrap();
                            mine.iter_mut().for_each(Vec::clear);
                            if !done {
                                me.records += parse_range(b, &mut i, end, lookup, &mut me.slots, |g, start, len| {
                                    let (owner, local) = route[g];
                                    let bucket = &mut mine[owner];
                                    bucket.push((local, intern.id(b, start, len)));
                                    // The round ends when any bucket is full.
                                    bucket.len() < cap
                                });
                                if i >= end {
                                    done = true;
                                    me.time = t.elapsed();
                                    finished.fetch_add(1, Ordering::Relaxed);
                                    // Pairwise end check (e1): the parse must stop exactly on the cut.
                                    exact.fetch_and(i == end, Ordering::Relaxed);
                                }
                            }
                        }
                        barrier.wait();
                        // Every worker that finishes did so before this barrier, and none can
                        // finish again before the next one, so all workers read the same value.
                        let all_done = finished.load(Ordering::Relaxed) == workers;
                        for theirs in buckets {
                            for &(local, id) in &theirs.read().unwrap()[w] {
                                me.owned[local as usize].insert(id);
                            }
                        }
                        if w == 0 {
                            // Retired slot generations would otherwise live to the end of the run.
                            // Between the two barriers no worker is inside a lookup: every lookup ran
                            // in parse_range before the first one. Measured (sdrig, this VM): M 20.3 ->
                            // 19.5 MiB, T unchanged; most growth happens in round 1, before the first
                            // free, so those retired generations still overlap the peak.
                            // SAFETY: the two barriers bracket this call, and lookups happen only
                            // outside that window, so none is in flight or can start before it returns.
                            unsafe { intern.free_retired() };
                        }
                        me.rounds += 1;
                        barrier.wait();
                        if all_done {
                            break me;
                        }
                    }
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("parse worker panicked")).collect()
    });

    let distinct = (0..groups).map(|g| results[g % workers].owned[g / workers].len()).collect();
    Pass2 {
        records: results.iter().map(|r| r.records).sum(),
        times: results.iter().map(|r| r.time).collect(),
        rounds: results[0].rounds,
        slots: results.into_iter().map(|r| r.slots).collect(),
        distinct,
        exact: exact.load(Ordering::Relaxed),
    }
}
