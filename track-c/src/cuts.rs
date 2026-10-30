//! Record-start cuts for N workers, the two-pass way: records.csv is split into N equal byte
//! ranges; pass 1 scans every range in parallel from both start states, outside quotes (RS) and
//! inside quotes (IQ), and the true start state of each range is then composed from range 0's.
//! The state is the quote parity: on in-grammar data every `"` toggles it (a `""` escape toggles
//! twice), and an LF is a terminator exactly when it is outside quotes (c-join.md:33-35).

/// What pass 1 learns about one byte range, for both start states.
#[derive(Default, Clone, Copy)]
struct RangeScan {
    /// First LF outside quotes if the range starts outside quotes (RS).
    first_lf_rs: Option<usize>,
    /// First LF outside quotes if the range starts inside quotes (IQ).
    first_lf_iq: Option<usize>,
    /// Whether the range holds an odd number of quotes, i.e. flips the state it starts in.
    flips: bool,
}

/// Transition-only scan of `b[lo..hi]`. Both start states advance in lockstep over one parity bit:
/// they stay complements, so they never coincide, and once each has seen its first LF only the
/// quote count matters for the rest of the range.
fn scan_range(b: &[u8], lo: usize, hi: usize) -> RangeScan {
    let mut scan = RangeScan::default();
    let mut odd = false; // quotes seen so far in the range, mod 2
    let mut i = lo;
    while i < hi && (scan.first_lf_rs.is_none() || scan.first_lf_iq.is_none()) {
        match b[i] {
            b'"' => odd = !odd,
            // From RS an LF is outside quotes after an even count; from IQ after an odd one.
            b'\n' if !odd => scan.first_lf_rs = scan.first_lf_rs.or(Some(i)),
            b'\n' => scan.first_lf_iq = scan.first_lf_iq.or(Some(i)),
            _ => {}
        }
        i += 1;
    }
    let rest_quotes = b[i..hi].iter().filter(|&&c| c == b'"').count();
    scan.flips = odd ^ (rest_quotes % 2 == 1);
    scan
}

/// Record starts for `workers` ranges: `starts[k]..starts[k + 1]` holds the records worker k parses,
/// and `starts[workers]` is the end of the file. `first` is where the first record starts (past the
/// header, which holds no quotes); a range with no terminator outside quotes gets no records.
pub fn record_starts(b: &[u8], first: usize, workers: usize) -> Vec<usize> {
    // Equal byte ranges: len / workers each (rounded up), the last one shorter.
    let chunk = b.len().div_ceil(workers).max(1);
    let bounds: Vec<(usize, usize)> =
        (0..workers).map(|k| ((k * chunk).min(b.len()), ((k + 1) * chunk).min(b.len()))).collect();
    let scans: Vec<RangeScan> = std::thread::scope(|s| {
        let handles: Vec<_> = bounds.iter().map(|&(lo, hi)| s.spawn(move || scan_range(b, lo, hi))).collect();
        handles.into_iter().map(|h| h.join().expect("scan worker panicked")).collect()
    });

    // Compose: range 0 starts outside quotes (the file starts with the BOM or the header); each
    // range's true start state is the previous one's, flipped by the previous range's parity.
    let mut starts = vec![b.len(); workers + 1];
    let mut inside = false;
    for (k, scan) in scans.iter().enumerate() {
        let lf = if inside { scan.first_lf_iq } else { scan.first_lf_rs };
        // A record starts just past a terminator, so range k's first record starts past its first
        // LF outside quotes (a BOM there is skipped by the parser).
        starts[k] = if k == 0 { first } else { lf.map_or(b.len(), |p| (p + 1).max(first)) };
        inside ^= scan.flips;
    }
    // A range without a terminator outside quotes starts where the next one does.
    for k in (0..workers).rev() {
        starts[k] = starts[k].min(starts[k + 1]);
    }
    starts
}
