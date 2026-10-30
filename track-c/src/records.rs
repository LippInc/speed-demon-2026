//! records.csv: the grammar of c-join.md:17-30, parsed one byte range per worker. Each record folds
//! into the worker's own per-group slot, and its name is handed to the caller (pass2.rs).

use crate::regions::Lookup;

const BOM: &[u8] = b"\xEF\xBB\xBF";

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
        if other.count == 0 {
            return;
        }
        if self.count == 0 {
            *self = other.clone();
            return;
        }
        self.count += other.count;
        self.id_sum += other.id_sum;
        self.amount_sum += other.amount_sum;
        self.amount_min = self.amount_min.min(other.amount_min);
        self.amount_max = self.amount_max.max(other.amount_max);
        self.name_bytes += other.name_bytes;
        self.note_bytes += other.note_bytes;
    }
}

/// One text field (name or note) starting at `i`, which is followed by a comma.
/// Returns where the raw bytes between the quotes start and how many there are (escapes kept),
/// the value's length once `""` is unescaped, and the index just past the comma.
#[inline]
fn text_field(b: &[u8], i: usize) -> (usize, usize, u64, usize) {
    if b[i] != b'"' {
        // Unquoted: everything up to the comma (c-join.md:28).
        let end = i + b[i..].iter().position(|&c| c == b',').expect("unterminated field");
        return (i, end - i, (end - i) as u64, end + 1);
    }
    // Quoted: ends at the first `"` not part of a `""` pair (c-join.md:33). Commas, LF and BOM
    // bytes inside are data, so they never stop the scan.
    let start = i + 1;
    let mut j = start;
    let mut escapes = 0u64;
    loop {
        let q = j + b[j..].iter().position(|&c| c == b'"').expect("unterminated quoted field");
        if b.get(q + 1) == Some(&b'"') {
            escapes += 1;
            j = q + 2;
        } else {
            return (start, q - start, (q - start) as u64 - escapes, q + 2);
        }
    }
}

/// An unsigned decimal ending at `stop`; returns it and the index just past `stop`.
#[inline]
fn uint_until(b: &[u8], mut i: usize, stop: u8) -> (u64, usize) {
    let mut n = 0u64;
    while b[i] != stop {
        n = n * 10 + (b[i] - b'0') as u64;
        i += 1;
    }
    (n, i + 1)
}

/// `-?[0-9]+\.[0-9]{2}` as integer cents (c-join.md:24, 37); returns it and the index just past it.
#[inline]
fn amount(b: &[u8], i: usize) -> (i64, usize) {
    let negative = b[i] == b'-';
    let (whole, i) = uint_until(b, i + negative as usize, b'.');
    let cents = whole as i64 * 100 + ((b[i] - b'0') * 10 + (b[i + 1] - b'0')) as i64;
    (if negative { -cents } else { cents }, i + 2)
}

#[inline]
pub fn skip_bom(b: &[u8], i: usize) -> usize {
    if b[i..].starts_with(BOM) { i + BOM.len() } else { i }
}

/// Index just past the header: a BOM may start the file, and the header holds no quotes and ends
/// at the first LF (c-join.md:18-19, 43).
pub fn header_end(b: &[u8]) -> usize {
    let i = skip_bom(b, 0);
    i + b[i..].iter().position(|&c| c == b'\n').map_or(b.len() - i, |p| p + 1)
}

/// Parses the records that start in `*i..end` (`*i` is a record start; the last record may run past
/// `end`) into `slots`, indexed by the lookup's group, and calls `on_name(group, raw_start, raw_len)`
/// with each record's raw name. Stops after a record whose `on_name` returns false, leaving `*i` at
/// the next record start, so the caller can resume. Returns the records parsed.
pub fn parse_range<L: Lookup + ?Sized>(
    b: &[u8],
    i: &mut usize,
    end: usize,
    lookup: &L,
    slots: &mut [Slot],
    mut on_name: impl FnMut(usize, usize, usize) -> bool,
) -> u64 {
    let mut records = 0;
    while *i < end {
        // A BOM directly after a terminator is skipped (c-join.md:36).
        let at = skip_bom(b, *i);
        let (id, next) = uint_until(b, at, b',');
        let (name_start, name_raw_len, name_len, next) = text_field(b, next);
        let (_, _, note_len, next) = text_field(b, next);
        let (region_id, next) = uint_until(b, next, b',');
        let (cents, next) = amount(b, next);
        // Terminator: CRLF, LF, or end of file (c-join.md:21).
        *i = match b.get(next) {
            Some(b'\r') => next + 2,
            Some(b'\n') => next + 1,
            _ => next,
        };

        let group = lookup.group(region_id as usize);
        let s = &mut slots[group];
        if s.count == 0 {
            // min and max start from the slot's first amount, never from 0.
            s.amount_min = cents;
            s.amount_max = cents;
        }
        s.count += 1;
        s.id_sum += id;
        s.amount_sum += cents;
        s.amount_min = s.amount_min.min(cents);
        s.amount_max = s.amount_max.max(cents);
        s.name_bytes += name_len;
        s.note_bytes += note_len;
        records += 1;
        if !on_name(group, name_start, name_raw_len) {
            break;
        }
    }
    records
}
