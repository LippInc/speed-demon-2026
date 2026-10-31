//! regions.csv: `region_id,country` lines, LF-terminated, ids unique and ascending, country `[A-Z]{3}`
//! (c-join.md:58-59). Parsed into a dense table region id -> group index; group 0 is `_UNMATCHED`.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU16, AtomicUsize, Ordering::*};

use crate::records::fold;

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

/// The region id of the line that ends at the LF `lf`, the previous line's LF being `prev`. The line
/// shape is fixed (c-join.md:58-59): the country is the 3 bytes before the LF and the comma is at
/// `lf - 4`, so the id is the `lf - 5 - prev` digits before that comma (at most 6: ids <= 999,999,
/// c-join.md:60). They are the top bytes of the 8 bytes ending at the comma, which lie inside the
/// file (the 18-byte header precedes every line), and fold in one SSE multiply-add chain; a longer
/// id (beyond the spec, but the kit has one) folds byte by byte.
#[inline(always)]
fn id(b: &[u8], prev: usize, lf: usize) -> usize {
    let len = (lf - 5 - prev) as u32;
    assert!(b[lf - 4] == b',' && len > 0, "bad line");
    if len > 7 {
        return b[prev + 1..lf - 4]
            .iter()
            .fold(0, |n, &d| n * 10 + (d - b'0') as usize);
    }
    let x = u64::from_le_bytes(b[lf - 12..lf - 4].try_into().unwrap()) ^ 0x3030_3030_3030_3030;
    fold(x >> (64 - 8 * len), len) as usize
}

/// Calls `line(prev, lf)` for every LF at `lf` in `from..to`, `prev` being the LF before it. A
/// per-byte loop is enough here: a worker's piece is 64 KiB (PIECE), and the AVX2 scan that replaced
/// it (c-47 s1) only paid when one thread parsed the whole file.
fn each_line(b: &[u8], from: usize, to: usize, mut line: impl FnMut(usize, usize)) {
    let mut prev = b[..from].iter().rposition(|&c| c == b'\n').unwrap_or(0);
    for lf in from..to {
        if b[lf] == b'\n' {
            line(prev, lf);
            prev = lf;
        }
    }
}

/// Bytes of regions.csv per piece a worker claims, a design constant: about 34 pieces for the 2.2 MB
/// file, so the workers spawned last still find work.
const PIECE: usize = 1 << 16;

/// regions.csv parsed by every worker at once (c-47 s2): they claim pieces of the file from a
/// counter, and a line belongs to the piece that holds its LF; ids are unique, so no two lines store
/// to one entry. Groups are numbered in first-seen order under a lock, taken only when a code has no
/// group yet (240 times at full scale); the output is sorted by key, so the numbering never shows.
pub struct Build<'a> {
    b: &'a [u8],
    /// Index just past the header `region_id,country`.
    first: usize,
    /// Region id -> group, one byte each: the spec has 240 countries (c-join.md:60).
    table: Box<[AtomicU8]>,
    /// Country code [A-Z]{3} (c-join.md:59) as its index in 26^3 -> group, 0 while it has none.
    code_group: Box<[AtomicU16]>,
    keys: Mutex<Vec<Vec<u8>>>,
    next: AtomicUsize,
    done: AtomicUsize,
}

impl<'a> Build<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        // c-57: an empty regions.csv (outside the grammar, kit case c14 madv-empty-regions) maps nothing instead of aborting.
        let first = b
            .iter()
            .position(|&c| c == b'\n')
            .map_or(b.len(), |p| p + 1);
        // Ids are ascending (c-join.md:59), so the last line (it ends with LF, c-join.md:58) holds
        // the largest and sizes the table: ids 0..=max, plus the group-0 entry every larger id is
        // clamped to (`group`). Zeroed memory is fresh pages, which the workers fault in.
        let last = b
            .len()
            .checked_sub(1)
            .and_then(|e| b[..e].iter().rposition(|&c| c == b'\n'));
        let max = last.map_or(0, |prev| id(b, prev, b.len() - 1));
        // SAFETY: all-zero bytes are valid atomics (0).
        let (table, code_group) = unsafe {
            let table = Box::new_zeroed_slice(max + 2).assume_init();
            (table, Box::new_zeroed_slice(26 * 26 * 26).assume_init())
        };
        let keys = Mutex::new(vec![b"_UNMATCHED".to_vec()]);
        let (next, done) = (AtomicUsize::new(0), AtomicUsize::new(0));
        Build {
            b,
            first,
            table,
            code_group,
            keys,
            next,
            done,
        }
    }

    /// Fills the table from pieces until none is left unclaimed, then waits for the other workers'
    /// pieces (each finished piece is counted with Release, read here with Acquire). Returns the table
    /// and the group count.
    pub fn ready(&self) -> (&[AtomicU8], usize) {
        let (b, pieces) = (self.b, (self.b.len() - self.first).div_ceil(PIECE));
        let mut k = self.next.fetch_add(1, Relaxed);
        while k < pieces {
            let from = self.first + k * PIECE;
            each_line(b, from, b.len().min(from + PIECE), |prev, lf| {
                let c = &b[lf - 3..lf];
                let code = (c[0] as usize * 676 + c[1] as usize * 26 + c[2] as usize)
                    .wrapping_sub(65 * 703);
                // A byte outside A-Z indexes out of range or another code; the shape is the spec's.
                let mut group = self.code_group[code].load(Relaxed);
                if group == 0 {
                    group = self.new_group(&self.code_group[code], c);
                }
                self.table[id(b, prev, lf)].store(group as u8, Relaxed);
            });
            self.done.fetch_add(1, Release);
            k = self.next.fetch_add(1, Relaxed);
        }
        while self.done.load(Acquire) < pieces {
            std::hint::spin_loop();
        }
        let groups = self.keys.lock().unwrap().len();
        assert!(groups <= 256, "over 255 countries");
        (&self.table, groups)
    }

    /// Numbers a code's group, unless another worker did first.
    #[cold]
    fn new_group(&self, cell: &AtomicU16, code: &[u8]) -> u16 {
        let mut keys = self.keys.lock().unwrap();
        if cell.load(Relaxed) == 0 {
            cell.store(keys.len() as u16, Relaxed);
            keys.push(code.to_vec());
        }
        cell.load(Relaxed)
    }

    /// Each group's output key (index 0 is `_UNMATCHED`).
    pub fn keys(self) -> Vec<Vec<u8>> {
        self.keys.into_inner().unwrap()
    }
}
