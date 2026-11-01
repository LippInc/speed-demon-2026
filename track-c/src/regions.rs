//! regions.csv: `region_id,country` lines, LF-terminated, ids unique and ascending, country `[A-Z]{3}`
//! (c-join.md:58-59). Parsed into a dense table region id -> group index; group 0 is `_UNMATCHED`.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering::*};

/// Group of a record's region id; ids the table does not hold (holes, or above its range) are group
/// 0. The table's last entry (one past the largest region id) is group 0, and every id above the
/// range is clamped to it: a `min` (one cmov) instead of a bounds check and its branch. Read only
/// after `Build::ready`, so the relaxed load is a plain load.
#[inline(always)]
pub fn group(table: &[AtomicU8], id: u64) -> usize {
    // SAFETY: the table is never empty (sized largest id + 2), so the clamp is in bounds.
    unsafe { table.get_unchecked((id as usize).min(table.len() - 1)) }.load(Relaxed) as usize
}

/// The start of the first line that begins after `at`: just past the first LF at or after it.
fn line_start(b: &[u8], at: usize) -> usize {
    let lf = b[at..].iter().position(|&c| c == b'\n');
    lf.map_or(b.len(), |p| at + p + 1)
}

/// A line `<id>,<code>` and its LF as the region id, the country code (c-join.md:58-59) packed
/// big-endian into a u32 (`AAA` = 0x414141: bytewise output order is then integer order, since
/// `_UNMATCHED`'s key starts with `_` (0x5F), above every letter 0x41..0x5A), and the code's index
/// in 26^3 (676 = 26^2; a byte outside A-Z indexes another code, the shape is the spec's). The id
/// is folded digit by digit, whatever its length (the spec's reach 6 digits, longer ones fold the
/// same way): 200,000 lines, shared out over the workers.
fn parse_line(line: &[u8]) -> (usize, u32, usize) {
    let [digits @ .., b',', a, b, c, b'\n'] = line else { panic!("bad regions.csv line") };
    let id = digits.iter().fold(0, |n, &d| n * 10 + (d - b'0') as usize);
    let letter = |x: &u8| (*x - b'A') as usize;
    let code = (*a as u32) << 16 | (*b as u32) << 8 | *c as u32;
    (id, code, letter(a) * 676 + letter(b) * 26 + letter(c))
}

/// regions.csv parsed by every worker at once (c-47 s2): they claim pieces of the file from a
/// counter, and a line belongs to the piece that holds the LF before it (the header's line to none);
/// ids are unique, so no two lines store to one entry. Groups are numbered in first-seen order, once
/// per code in its code's `OnceLock` (240 codes at full scale); the output is sorted by key, so the
/// numbering never shows.
#[derive(Default)]
pub struct Build<'a> {
    b: &'a [u8],
    /// Region id -> group, one byte each: the spec has 240 countries (c-join.md:60).
    table: Box<[AtomicU8]>,
    /// Country code [A-Z]{3} (c-join.md:59) as its index in 26^3 -> its group, once it has one.
    code_group: Box<[OnceLock<u8>]>,
    /// Each group's output key as `parse_line` packs it, written once when the group is numbered;
    /// read only after the workers joined, so the relaxed loads are plain loads.
    pub(crate) keys: Box<[AtomicU32]>,
    /// The next group number to hand out (0 is `_UNMATCHED`).
    groups: AtomicUsize,
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
        let mut build = Build::default();
        build.b = b;
        let n = last.map_or(0, |l| parse_line(l).0) + 2;
        // SAFETY: all-zero bytes are valid atomics (0).
        build.table = unsafe { Box::new_zeroed_slice(n).assume_init() };
        build.code_group = (0..26 * 26 * 26).map(|_| OnceLock::new()).collect();
        build.keys = unsafe { Box::new_zeroed_slice(256).assume_init() };
        // Group 0 is `_UNMATCHED`, whose packed key sorts above every code (parse_line).
        build.keys[0].store(0x5F_55_4E, Relaxed);
        build
    }

    /// Fills the table from pieces until none is left unclaimed, then waits for the other workers'
    /// pieces (each finished piece is counted with Release, read here with Acquire). Returns the
    /// table: the spec has at most 240 countries (c-join.md:60), so every group fits a byte, and
    /// `keys` holds one entry per group. A piece is 64 KiB of the file, a design constant: about 34
    /// pieces for the 2.2 MB file, so the workers spawned last still find work.
    pub fn ready(&self) -> &[AtomicU8] {
        let b = self.b;
        while let from = self.next.fetch_add(1 << 16, Relaxed)
            && from < b.len()
        {
            let to = b.len().min(from + (1 << 16));
            for line in b[line_start(b, from)..line_start(b, to)].split_inclusive(|&c| c == b'\n') {
                let (id, code, index) = parse_line(line);
                // First-seen numbering: group numbers start at 1 (0 is `_UNMATCHED`), and the
                // spec's 240 countries (c-join.md:60) fit a byte.
                let group = self.code_group[index].get_or_init(|| {
                    let g = 1 + self.groups.fetch_add(1, Relaxed);
                    self.keys[g].store(code, Relaxed);
                    g as u8
                });
                self.table[id].store(*group, Relaxed);
            }
            self.done.fetch_add(to - from, Release);
        }
        while self.done.load(Acquire) < b.len() {}
        &self.table
    }
}
