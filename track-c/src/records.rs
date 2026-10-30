//! records.csv: one sequential pass over the grammar of c-join.md:17-30, folding each record into
//! its group's aggregates as it is parsed.

use crate::names::{NameIds, NameSet};
use crate::regions::Lookup;

const BOM: &[u8] = b"\xEF\xBB\xBF";

/// Running aggregates of one output line (c-join.md:109-118).
#[derive(Default)]
pub struct Group {
    pub count: u64,
    pub id_sum: u64,
    pub amount_sum: i64,
    pub amount_min: i64,
    pub amount_max: i64,
    pub names: NameSet,
    pub name_bytes: u64,
    pub note_bytes: u64,
}

/// One text field (name or note) starting at `i`, which is followed by a comma.
/// Returns the raw bytes between the quotes (escapes kept), the value's length once `""` is
/// unescaped, and the index just past the comma.
#[inline]
fn text_field(b: &'static [u8], i: usize) -> (&'static [u8], u64, usize) {
    if b[i] != b'"' {
        // Unquoted: everything up to the comma (c-join.md:28).
        let end = i + b[i..].iter().position(|&c| c == b',').expect("unterminated field");
        return (&b[i..end], (end - i) as u64, end + 1);
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
            return (&b[start..q], (q - start) as u64 - escapes, q + 2);
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
fn skip_bom(b: &[u8], i: usize) -> usize {
    if b[i..].starts_with(BOM) { i + BOM.len() } else { i }
}

/// Parses every record of `b` into `groups` (indexed by the lookup's group); returns the record count.
pub fn parse<L: Lookup + ?Sized>(b: &'static [u8], lookup: &L, groups: &mut [Group], names: &mut NameIds) -> u64 {
    // A BOM may start the file; the header holds no quotes and ends at the first LF (c-join.md:18-19, 43).
    let mut i = skip_bom(b, 0);
    i += b[i..].iter().position(|&c| c == b'\n').map_or(b.len() - i, |p| p + 1);
    let mut records = 0;
    while i < b.len() {
        // A BOM directly after a terminator is skipped (c-join.md:36).
        i = skip_bom(b, i);
        let (id, next) = uint_until(b, i, b',');
        let (name, name_len, next) = text_field(b, next);
        let (_, note_len, next) = text_field(b, next);
        let (region_id, next) = uint_until(b, next, b',');
        let (cents, next) = amount(b, next);
        // Terminator: CRLF, LF, or end of file (c-join.md:21).
        i = match b.get(next) {
            Some(b'\r') => next + 2,
            Some(b'\n') => next + 1,
            _ => next,
        };

        let g = &mut groups[lookup.group(region_id as usize)];
        if g.count == 0 {
            // min and max start from the group's first amount, never from 0.
            g.amount_min = cents;
            g.amount_max = cents;
        }
        g.count += 1;
        g.id_sum += id;
        g.amount_sum += cents;
        g.amount_min = g.amount_min.min(cents);
        g.amount_max = g.amount_max.max(cents);
        g.names.insert(names.id(name));
        g.name_bytes += name_len;
        g.note_bytes += note_len;
        records += 1;
    }
    records
}
