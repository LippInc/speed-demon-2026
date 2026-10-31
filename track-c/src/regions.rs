//! regions.csv: `region_id,country` lines, LF-terminated, ids unique and ascending, country `[A-Z]{3}`
//! (c-join.md:58-59). Parsed into a dense table region id -> group index; group 0 is `_UNMATCHED`.

use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering::*};
use std::sync::{Mutex, OnceLock};

/// Group of a record's region id; ids the table does not hold (holes, or above its range) are group
/// 0. The table's last entry (one past the largest region id) is group 0, and every id above the
/// range is clamped to it: a `min` (one cmov) instead of a bounds check and its branch. Read only
/// after `Build::ready`, so the relaxed load is a plain load.
#[inline(always)]
pub fn group(table: &[AtomicU8], region_id: u64) -> usize {
    // SAFETY: the table is never empty (sized largest id + 2), so the clamp is in bounds.
    let cell = unsafe { table.get_unchecked((region_id as usize).min(table.len() - 1)) };
    cell.load(Relaxed) as usize
}

/// Bytes of regions.csv per piece a worker claims, a design constant: about 34 pieces for the 2.2 MB
/// file, so the workers spawned last still find work.
const PIECE: usize = 1 << 16;

/// The start of the first line that begins after `at`: just past the first LF at or after it.
fn line_start(b: &[u8], at: usize) -> usize {
    let lf = b[at..].iter().position(|&c| c == b'\n');
    lf.map_or(b.len(), |p| at + p + 1)
}

/// A line `<id>,<code>` and its LF as the region id and the country code (c-join.md:58-59). The id
/// is folded digit by digit, whatever its length (the spec's reach 6 digits, longer ones fold the
/// same way): 200,000 lines, shared out over the workers.
fn parse_line(line: &[u8]) -> (usize, [u8; 3]) {
    let [digits @ .., b',', a, b, c, b'\n'] = line else {
        panic!("bad regions.csv line");
    };
    let id = digits.iter().fold(0, |n, &d| n * 10 + (d - b'0') as usize);
    (id, [*a, *b, *c])
}

/// regions.csv parsed by every worker at once (c-47 s2): they claim pieces of the file from a
/// counter, and a line belongs to the piece that holds the LF before it (the header's line to none);
/// ids are unique, so no two lines store to one entry. Groups are numbered in first-seen order, once
/// per code under a lock (240 times at full scale); the output is sorted by key, so the numbering
/// never shows.
#[derive(Default)]
pub struct Build<'a> {
    b: &'a [u8],
    /// Region id -> group, one byte each: the spec has 240 countries (c-join.md:60).
    table: Box<[AtomicU8]>,
    /// Country code [A-Z]{3} (c-join.md:59) as its index in 26^3 -> its group, once it has one.
    code_group: Box<[OnceLock<u8>]>,
    /// The countries' output keys in group order, from group 1 on (group 0 is `_UNMATCHED`).
    keys: Mutex<Vec<Vec<u8>>>,
    /// The first byte of the next unclaimed piece, and how many bytes of pieces are filled in.
    next: AtomicUsize,
    done: AtomicUsize,
}

impl<'a> Build<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        // Ids are ascending (c-join.md:59), so the last line holds the largest and sizes the table:
        // ids 0..=max, plus the group-0 entry every larger id is clamped to (`group`). A file with no
        // line after the header (outside the grammar, kit case c14) maps nothing. Zeroed memory is
        // fresh pages, which the workers fault in.
        let body = &b[line_start(b, 0)..];
        let last = body.split_inclusive(|&c| c == b'\n').next_back();
        let mut build = Build { b, ..Default::default() };
        // SAFETY: all-zero bytes are valid atomics (0).
        build.table = unsafe { Box::new_zeroed_slice(last.map_or(0, |l| parse_line(l).0) + 2).assume_init() };
        build.code_group = (0..26 * 26 * 26).map(|_| OnceLock::new()).collect();
        build
    }

    /// Fills the table from pieces until none is left unclaimed, then waits for the other workers'
    /// pieces (each finished piece is counted with Release, read here with Acquire). Returns the table:
    /// at most 255 countries, so every group fits a byte and a slot array of 256.
    pub fn ready(&self) -> &[AtomicU8] {
        let b = self.b;
        while let from = self.next.fetch_add(PIECE, Relaxed)
            && from < b.len()
        {
            let to = b.len().min(from + PIECE);
            for line in b[line_start(b, from)..line_start(b, to)].split_inclusive(|&c| c == b'\n') {
                let (id, code) = parse_line(line);
                // A byte outside A-Z indexes out of range or another code; the shape is the spec's.
                let index = code.iter().fold(0, |n, &c| n * 26 + (c - b'A') as usize);
                let group = self.code_group[index].get_or_init(|| {
                    let mut keys = self.keys.lock().unwrap();
                    keys.push(code.to_vec());
                    assert!(keys.len() < 256, "over 255 countries");
                    keys.len() as u8
                });
                self.table[id].store(*group, Relaxed);
            }
            self.done.fetch_add(to - from, Release);
        }
        while self.done.load(Acquire) < b.len() {
            std::hint::spin_loop();
        }
        &self.table
    }

    /// Each group's output key (index 0 is `_UNMATCHED`).
    pub fn keys(self) -> Vec<Vec<u8>> {
        let mut keys = self.keys.into_inner().unwrap();
        keys.insert(0, b"_UNMATCHED".to_vec());
        keys
    }
}
