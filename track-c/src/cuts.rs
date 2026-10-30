//! Record-start cuts for N workers: records.csv is split into N equal byte ranges, and each range
//! after the first starts at its first record start, found by a short forward search — no parallel
//! scan of the file.

use crate::records::BOM;

/// Record starts for `workers` ranges: `starts[k]..starts[k + 1]` holds the records worker k parses,
/// and `starts[workers]` is the end of the file. `first` is where the first record starts (past the
/// header, which holds no quotes); a range with no record start gets no records.
///
/// e1: by the grammar (c-join.md:17-36) a record opens with a digit (the id) or a BOM right after
/// its terminator (CRLF or LF, both ending in LF), and no quoted field ever holds an LF followed by
/// a digit or BOM (names hold no LF at all, notes are lowercase words), so LF + digit/BOM marks a
/// record start and each start is one short search from the range's offset instead of pass 1's full
/// quote-parity scan of every byte. Pass 2 checks pairwise that every worker stopped exactly on its
/// cut, and main reruns the whole file on one worker if not, so a wrong cut can never change the
/// answer. Measured (this VM, full data, 12 workers, interleaved medians of 11, warm page cache):
/// pass1 7.3 -> 0.1 ms, total 112.6 -> 109.4 ms.
pub fn record_starts(b: &[u8], first: usize, workers: usize) -> Vec<usize> {
    // Equal byte ranges: len / workers each (rounded up), the last one shorter.
    let chunk = b.len().div_ceil(workers).max(1);
    let mut starts = vec![b.len(); workers + 1];
    starts[0] = first;
    for k in 1..workers {
        let lo = (k * chunk).min(b.len());
        starts[k] = (lo..b.len())
            .find(|&p| b[p - 1] == b'\n' && (b[p].is_ascii_digit() || b[p..].starts_with(BOM)))
            .unwrap_or(b.len());
    }
    // A range with no record start begins where the next one does.
    for k in (0..workers).rev() {
        starts[k] = starts[k].min(starts[k + 1]);
    }
    starts
}
