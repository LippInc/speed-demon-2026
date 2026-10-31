//! records.csv: the grammar of c-join.md:17-30, parsed one chunk per call over the padded mapping
//! (main.rs `map_padded`). Each record folds into the worker's own per-group slot.
//!
//! Every load here is unchecked: the mapping is followed by GUARD bytes repeating `"` `,` LF, so a
//! quote or comma search started anywhere up to the guard's middle stops inside it, and a digit run
//! stops at its first `"`. A record that would run past the end of the file (only a garbage record,
//! from a cut inside a quoted field) is caught once per record, by its end position, never per byte.

use std::arch::x86_64::*;
use std::sync::atomic::AtomicU8;

use crate::regions::group;

const BOM: &[u8] = b"\xEF\xBB\xBF";

/// Guard bytes after the file's last byte (`"` `,` LF repeated). A record's six searches each start at
/// most 3 bytes past the previous one's stop, so from the end of the file they stay within 24 bytes;
/// 96 leaves every 32-byte search a hit, and the reservation behind it (4 KiB) covers every load.
pub const GUARD: usize = 96;

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
    /// An empty slot: the minimum and maximum start at the extremes, so the first amount replaces both
    /// and an empty slot merges as a no-op.
    pub fn new() -> Slot {
        Slot {
            amount_min: i64::MAX,
            amount_max: i64::MIN,
            ..Default::default()
        }
    }

    /// Folds another worker's slot for the same group into this one.
    pub fn merge(&mut self, other: &Slot) {
        self.count += other.count;
        self.id_sum += other.id_sum;
        self.amount_sum += other.amount_sum;
        self.amount_min = self.amount_min.min(other.amount_min);
        self.amount_max = self.amount_max.max(other.amount_max);
        self.name_bytes += other.name_bytes;
        self.note_bytes += other.note_bytes;
    }
}

/// An unaligned little-endian load of `T` at `p + i`.
///
/// # Safety
/// `p + i .. p + i + size_of::<T>()` must lie inside the padded mapping.
#[inline(always)]
pub(crate) unsafe fn load<T>(p: *const u8, i: usize) -> T {
    unsafe { std::ptr::read_unaligned(p.add(i) as *const T) }
}

/// Index of the first `c` at or after `i`: 32 bytes per step (AVX2 compare, movemask, trailing zeros).
/// Names and notes nearly always fit one step (c-join.md 2.2), so this is one load and one branch.
#[inline(always)]
unsafe fn find(p: *const u8, mut i: usize, c: u8) -> usize {
    // SAFETY (caller's): a `c` (quote or comma, both in the guard) lies before the reservation's end.
    let needle = unsafe { _mm256_set1_epi8(c as i8) };
    loop {
        let m = unsafe { _mm256_movemask_epi8(_mm256_cmpeq_epi8(load(p, i), needle)) } as u32;
        if m != 0 {
            return i + m.trailing_zeros() as usize;
        }
        i += 32;
    }
}

/// One text field (name or note) at `i`: where its raw bytes start (after the opening quote, escapes
/// kept), how many there are, the value's length once `""` is unescaped, and the index past its comma.
#[inline(always)]
unsafe fn text(p: *const u8, i: usize) -> (usize, usize, u64, usize) {
    unsafe {
        if *p.add(i) != b'"' {
            // Unquoted: everything up to the comma (c-join.md:28); about 1 % of names, 3 % of notes.
            let end = find(p, i, b',');
            return (i, end - i, (end - i) as u64, end + 1);
        }
        // Quoted: ends at the first `"` not part of a `""` pair (c-join.md:33). Commas, LF and BOM
        // bytes inside are data. The loop runs again only on an escape.
        let start = i + 1;
        let mut q = find(p, start, b'"');
        let mut escapes = 0;
        while *p.add(q + 1) == b'"' {
            escapes += 1;
            q = find(p, q + 2, b'"');
        }
        (start, q - start, (q - start - escapes) as u64, q + 2)
    }
}

/// The 8 bytes at `i` with each digit turned into its value, and the number of digits they start with
/// (8 when all are digits): a byte is a digit iff, XORed with `0`, it is 0..=9 (no high bit, none
/// after adding 0x76).
#[inline(always)]
unsafe fn digits(p: *const u8, i: usize) -> (u64, u32) {
    let x = unsafe { load::<u64>(p, i) } ^ 0x3030_3030_3030_3030;
    let non_digits = (x.wrapping_add(0x7676_7676_7676_7676) | x) & 0x8080_8080_8080_8080;
    (x, non_digits.trailing_zeros() / 8)
}

/// Folds the `len` (0 to 8) digit values at the bottom of `x` into their number. The digits move
/// to the top bytes (two shifts, so `len` 0 gives 0 instead of an over-wide shift) and the zeros
/// below read as leading zeros; then SSE multiply-adds pair them up: digit pairs (x10 + 1), pairs of
/// those (x100 + 1), then the two 4-digit halves (x10000 + 1). Six vector instructions where the
/// scalar fold (three multiplies, shifts and masks) took about 13 and three 64-bit constants.
#[inline(always)]
fn fold(x: u64, len: u32) -> u64 {
    // SAFETY: SSE4.1 and SSSE3 are on at compile time (x86-64-v3).
    unsafe {
        let v = _mm_cvtsi64_si128(((x << 1) << (63 - 8 * len)) as i64);
        let v = _mm_maddubs_epi16(v, _mm_set1_epi16(0x010A)); // bytes 10, 1 per digit pair
        let v = _mm_madd_epi16(v, _mm_set1_epi32(0x0001_0064)); // 100, 1 per pair of pairs
        let v = _mm_madd_epi16(_mm_packus_epi32(v, v), _mm_set1_epi32(0x0001_2710)); // 10000, 1
        _mm_cvtsi128_si32(v) as u32 as u64
    }
}

/// An unsigned decimal at `i` ending at a comma; returns it and the index past the comma. One load
/// parses up to 7 digits and their comma (id and region id always fit: c-join.md 2.2); anything
/// else goes to `long_number`, off the hot path. The stop byte is read from the loaded word: shifted
/// out of range (8 digits) the word's first digit is never a comma.
#[inline(always)]
unsafe fn uint(p: *const u8, i: usize) -> (u64, usize) {
    let (x, len) = unsafe { digits(p, i) };
    if x.wrapping_shr(8 * len) as u8 == b',' ^ b'0' {
        return (fold(x, len), i + len as usize + 1);
    }
    let (n, stop) = unsafe { long_number(p, i) };
    (n, stop + 1)
}

/// The slow path of `uint` and `amount`: the digits one by one, a `.` skipped (so an amount comes
/// out in cents), up to the first other byte, which the caller's grammar makes the stop byte (a
/// garbage record may end elsewhere; its end check catches it). Returns the number and the index of
/// the stop byte. A BOM before the digits is where a record's id lands when a BOM follows its
/// terminator (c-join.md:36): skipped here, which took the BOM test off every record (777 BOMs in
/// 3.32M records, c-join.md:200).
#[cold]
#[inline(never)]
unsafe fn long_number(p: *const u8, mut i: usize) -> (u64, usize) {
    i += 3 * (unsafe { load::<u32>(p, i) } & 0xFF_FFFF == 0xBF_BBEF) as usize;
    let mut n = 0u64;
    // SAFETY: a run of digits and points ends at the guard's `"` at the latest.
    while let d @ (b'0'..=b'9' | b'.') = unsafe { *p.add(i) } {
        if d != b'.' {
            n = n.wrapping_mul(10).wrapping_add((d - b'0') as u64);
        }
        i += 1;
    }
    (n, i)
}

/// An amount `-?[0-9]+\.[0-9]{2}` at `i` as integer cents (c-join.md:24, 37); returns it and the index
/// of the byte after it. Up to 5 whole digits (the generator's range, c-join.md 2.2) one load holds the
/// whole part, the point and both fraction digits: by the grammar the first non-digit is the point,
/// whose byte is cut out (the bytes above it shifted down one) so the rest folds as one number. 6 or
/// more whole digits (beyond the spec) go to `long_number`.
#[inline(always)]
unsafe fn amount(p: *const u8, i: usize) -> (i64, usize) {
    let negative = unsafe { *p.add(i) } == b'-';
    let at = i + negative as usize;
    let (x, len) = unsafe { digits(p, at) };
    let (cents, next) = if len <= 5 {
        let below = unsafe { _bzhi_u64(!0, 8 * len) };
        let cents = fold(x & below | (x >> 8) & !below, len + 2);
        (cents, at + len as usize + 3)
    } else {
        unsafe { long_number(p, at) }
    };
    // The sign without a branch: two's complement negation when `negative` is 1.
    let n = negative as i64;
    ((cents as i64 ^ -n) + n, next)
}

/// Parses the records that start in `i..end` (`i` is a record start, the last record may run past
/// `end`) of the file `b` into `slots`, and calls `on_record(group, name start, raw name
/// length)` once each record is done (a closure inlined here). Returns where it stopped: the next
/// record start, or `usize::MAX` when a record ran past the end of the file (garbage from a wrong
/// cut, so the caller's end check fails and the pass reruns on one worker).
///
/// # Safety
/// The mapping must continue for GUARD bytes after `b` (main.rs `map_padded`), and `slots` must hold
/// 256 entries: every group `regions` holds is below that.
pub unsafe fn parse_chunk(
    b: &[u8],
    mut i: usize,
    end: usize,
    regions: &[AtomicU8],
    slots: &mut [Slot],
    mut on_record: impl FnMut(usize, usize, usize),
) -> usize {
    let len = b.len();
    assert!(end <= len);
    let p = b.as_ptr();
    // SAFETY: every load starts at most a few bytes past the guard's start (see GUARD) or inside the file.
    unsafe {
        while i < end {
            let (id, next) = uint(p, i);
            let (name_start, name_raw, name_len, next) = text(p, next);
            let (_, _, note_len, next) = text(p, next);
            let (region, next) = uint(p, next);
            let (cents, after) = amount(p, next);
            // A record past the end of the file is garbage (see the module comment): stop here.
            if after > len {
                return usize::MAX;
            }
            // Terminator: CRLF or LF (c-join.md:21), one byte plus one for a CR, no branch.
            i = after + 1 + (*p.add(after) == b'\r') as usize;

            let group = group(regions, region);
            let s = slots.get_unchecked_mut(group);
            s.count += 1;
            s.id_sum += id;
            s.amount_sum += cents;
            s.amount_min = s.amount_min.min(cents);
            s.amount_max = s.amount_max.max(cents);
            s.name_bytes += name_len;
            s.note_bytes += note_len;
            on_record(group, name_start, name_raw);
        }
    }
    // A last record without a terminator ends at the file's end (c-join.md:21): its terminator skip
    // above stepped one past it.
    i.min(len)
}

/// The first record start at or after `from` (`from` itself excluded unless it follows an LF), or the
/// end of the file: where a chunk begins (and its predecessor ends). `from` 0 finds the first record
/// after the header, which holds no quotes (c-join.md:18-19, 43).
///
/// e1: by the grammar (c-join.md:17-36) a record opens with a digit (the id) or a BOM right after
/// its terminator (CRLF or LF, both ending in LF). The generator's files (c-join.md 2.2) never hold
/// an LF followed by a digit or BOM inside a quoted field (names hold no LF at all, notes are
/// lowercase words), so LF + digit/BOM marks a record start. The grammar alone (2.1) allows one
/// inside quotes; then the cut is wrong, the previous chunk (whose start is right) does not stop
/// exactly on it, and main reruns the whole file on one worker, so a wrong cut never changes the answer.
pub fn record_start(b: &[u8], from: usize) -> usize {
    (from.max(1)..b.len())
        .find(|&p| b[p - 1] == b'\n' && (b[p].is_ascii_digit() || b[p..].starts_with(BOM)))
        .unwrap_or(b.len())
}
