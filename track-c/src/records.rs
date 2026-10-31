//! records.csv: the grammar of c-join.md:17-30, parsed one byte range per worker. Each record folds
//! into the worker's own per-group slot, and its name is handed to the caller (pass2.rs).

use crate::regions::Lookup;

pub(crate) const BOM: &[u8] = b"\xEF\xBB\xBF";

/// One worker's running aggregates of one output line (c-join.md:109-118), names_distinct aside.
#[derive(Default, Clone)]
pub struct Slot {
    pub count: u64,
    pub id_sum: u64,
    pub amount_sum: i64,
    pub amount_min: i64,
    pub amount_max: i64,
    pub name_bytes: u64,
    pub note_bytes: u64,
}

impl Slot {
    /// Folds another worker's slot for the same group into this one.
    pub fn merge(&mut self, other: &Slot) {
        if self.count == 0 {
            *self = other.clone();
        } else if other.count > 0 {
            self.count += other.count;
        self.id_sum += other.id_sum;
        self.amount_sum += other.amount_sum;
        self.amount_min = self.amount_min.min(other.amount_min);
        self.amount_max = self.amount_max.max(other.amount_max);
        self.name_bytes += other.name_bytes;
        self.note_bytes += other.note_bytes;
        }
    }
}

/// Index of the first `c` at or after `i`. Compares 32 bytes a step (AVX2 compare, movemask, trailing
/// zeros) instead of one byte a step; the last 31 bytes of the mapping go byte by byte, so nothing is
/// read past its end. Measured (this VM, 12 workers, full seed 1, interleaved medians): pass2+names
/// 294 -> 244 ms (c-10).
fn find(b: &[u8], mut i: usize, c: u8) -> usize {
    #[cfg(target_feature = "avx2")]
    while i + 32 <= b.len() {
        use std::arch::x86_64::*;
        // SAFETY: the 32 bytes at i lie inside `b`; AVX2 is on at compile time (x86-64-v3).
        let m = unsafe {
            let block = _mm256_loadu_si256(b.as_ptr().add(i) as *const __m256i);
            _mm256_movemask_epi8(_mm256_cmpeq_epi8(block, _mm256_set1_epi8(c as i8))) as u32
        };
        if m != 0 {
            return i + m.trailing_zeros() as usize;
        }
        i += 32;
    }
    // Not found (only in a range cut inside a quoted field): the end of the mapping.
    let found = b.get(i..).and_then(|r| r.iter().position(|&x| x == c));
    found.map_or(b.len(), |p| i + p)
}

/// One text field (name or note) starting at `i`, which is followed by a comma.
/// Returns where the raw bytes between the quotes start and how many there are (escapes kept),
/// the value's length once `""` is unescaped, and the index just past the comma.
fn text_field(b: &[u8], i: usize) -> (usize, usize, u64, usize) {
    if b.get(i) != Some(&b'"') {
        // Unquoted: everything up to the comma (c-join.md:28).
        let end = find(b, i, b',');
        return (i, end - i, (end - i) as u64, end + 1);
    }
    // Quoted: ends at the first `"` not part of a `""` pair (c-join.md:33). Commas, LF and BOM
    // bytes inside are data, so they never stop the scan.
    let (start, mut j) = (i + 1, i + 1);
    let mut escapes = 0u64;
    loop {
        let q = find(b, j, b'"');
        if b.get(q + 1) != Some(&b'"') {
            return (start, q - start, (q - start) as u64 - escapes, q + 2);
        }
        escapes += 1;
        j = q + 2;
    }
}

/// An unsigned decimal ending at `stop`; returns it and the index just past `stop`.
fn uint_until(b: &[u8], mut i: usize, stop: u8) -> (u64, usize) {
    let mut n = 0u64;
    while let Some(&d) = b.get(i).filter(|&&d| d != stop) {
        n = n * 10 + (d - b'0') as u64;
        i += 1;
    }
    (n, i + 1)
}

/// Parses the records that start in `*i..end` (`*i` is a record start; the last record may run past
/// `end`) into `slots`, indexed by the lookup's group, and calls `on_name(group, raw_start, raw_len)`
/// with each record's raw name. Stops after a record whose `on_name` returns false, leaving `*i` at
/// the next record start, so the caller can resume.
pub fn parse_range<L: Lookup + ?Sized>(
    b: &[u8],
    i: &mut usize,
    end: usize,
    lookup: &L,
    slots: &mut [Slot],
    mut on_name: impl FnMut(usize, usize, usize) -> bool,
) {
    while *i < end {
        // A BOM directly after a terminator is skipped (c-join.md:36).
        let at = if b[*i..].starts_with(BOM) { *i + 3 } else { *i };
        let (id, next) = uint_until(b, at, b',');
        let (name_start, name_raw_len, name_len, next) = text_field(b, next);
        let (_, _, note_len, next) = text_field(b, next);
        let (region_id, next) = uint_until(b, next, b',');
        // `-?[0-9]+\.[0-9]{2}` as integer cents (c-join.md:24, 37).
        let negative = b.get(next) == Some(&b'-');
        let (whole, next) = uint_until(b, next + negative as usize, b'.');
        let digits = b.get(next..next + 2);
        let frac = digits.map_or(0, |d| (d[0] - b'0') * 10 + (d[1] - b'0'));
        let cents = whole as i64 * 100 + frac as i64;
        let (cents, next) = (if negative { -cents } else { cents }, next + 2);
        // Terminator: CRLF or LF (c-join.md:21), skipped without a branch: one byte, plus one for a CR.
        // At the end of a file without a final terminator this is one past the end, which ends the
        // range all the same. Measured (this VM, 12 workers, full seed 1, interleaved medians of 9):
        // pass2+names 259 -> 255 ms (c-10), inside the noise; it is kept for being four lines shorter.
        *i = next + 1 + (b.get(next) == Some(&b'\r')) as usize;
        // A record that ran past the end of the file is garbage from a range cut inside a quoted
        // field (c-join.md 2.1 allows LF + digit there): it is dropped and `*i` is past `end`, so
        // pass 2's exact check fails and main reruns the file on one worker. Every read above is
        // bounds-checked for this case, so such a range ends here instead of aborting.
        if next > b.len() {
            break;
        }

        let group = lookup.group(region_id as usize);
        let s = &mut slots[group];
        // min and max start from the slot's first amount, never from 0.
        let first = s.count == 0;
        s.count += 1;
        s.id_sum += id;
        s.amount_sum += cents;
        (s.amount_min, s.amount_max) = if first {
            (cents, cents)
        } else {
            (cents.min(s.amount_min), cents.max(s.amount_max))
        };
        s.name_bytes += name_len;
        s.note_bytes += note_len;
        if !on_name(group, name_start, name_raw_len) {
            break;
        }
    }
}

/// The first record start at or after `from` (`from` itself excluded unless it follows an LF), or the
/// end of the file: where a chunk of pass 2 begins (and its predecessor ends).
///
/// e1: by the grammar (c-join.md:17-36) a record opens with a digit (the id) or a BOM right after
/// its terminator (CRLF or LF, both ending in LF). The generator's files (c-join.md 2.2) never hold
/// an LF followed by a digit or BOM inside a quoted field (names hold no LF at all, notes are
/// lowercase words), so LF + digit/BOM marks a record start and each start is one short search from
/// the chunk's offset instead of pass 1's full quote-parity scan of every byte. The grammar alone
/// (2.1) allows one inside quotes; then the cut is wrong, the previous chunk does not stop exactly
/// on it, and main reruns the whole file on one worker, so a wrong cut can never change the answer
/// (parse_range drops a garbage record that runs past the end of the file instead of aborting).
/// Measured (this VM, full data, 12 workers, interleaved medians of 11, warm page cache): pass1 7.3
/// -> 0.1 ms, total 112.6 -> 109.4 ms.
pub fn record_start(b: &[u8], from: usize) -> usize {
    (from.max(1)..b.len())
        .find(|&p| b[p - 1] == b'\n' && (b[p].is_ascii_digit() || b[p..].starts_with(BOM)))
        .unwrap_or(b.len())
}
