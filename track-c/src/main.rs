//! Track C (join): per-country aggregates of records.csv joined against regions.csv, as specified
//! in specs/c-join.md. Workers claim 256 KiB chunks of records.csv from a counter and parse them;
//! the region table is built by all workers together before their first chunk.

mod names;
mod pass2;
mod records;
mod regions;

use std::{env, fs::File, io::Write, os::fd::AsRawFd};

/// Integer cents as `-?[0-9]+\.[0-9]{2}`, never `-0.00` (c-join.md:131).
fn money(c: i64) -> String {
    let a = c.unsigned_abs();
    format!("{}{}.{:02}", if c < 0 { "-" } else { "" }, a / 100, a % 100)
}

// Read-only file mappings through libc's mmap (std links libc; no crate needed).
unsafe extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
}

/// Maps a whole file (records.csv, regions.csv) followed by GUARD bytes repeating `"` `,` LF
/// (records.rs), and returns the file's bytes; the guard follows them in memory. The mapping lives until exit, so it
/// is never unmapped. A private anonymous reservation of the file's length plus 4 KiB is
/// mapped first, then the file over its start (MAP_FIXED), so the bytes after the file are the file's
/// last page tail or the reservation, both private and zero; the guard is written there (at most one
/// page copied, 4 KiB of M). Every scan then stops at the guard, so the parse needs no bounds checks.
fn map_padded(path: &str) -> &'static [u8] {
    let f = File::open(path).expect("open input");
    let len = f.metadata().expect("stat").len() as usize;
    // SAFETY: fresh private mappings: the reservation, then the file over its first `len` bytes.
    unsafe {
        // prot 3 = PROT_READ | PROT_WRITE; flags 2 = MAP_PRIVATE, 0x20 = MAP_ANONYMOUS, 0x10 = MAP_FIXED.
        let base = mmap(std::ptr::null_mut(), len + 4096, 3, 0x22, -1, 0);
        // An empty file (outside 2.2, but kit case c14 madv-empty-* feeds one): the zeroed
        // reservation alone is the file's bytes; mmap of length 0 would fail.
        if len != 0 {
            assert!(mmap(base, len, 3, 0x12, f.as_raw_fd(), 0) == base);
        }
        (0..records::GUARD).for_each(|k| *base.add(len + k) = b"\",\n"[k % 3]);
        std::slice::from_raw_parts(base, len)
    }
}

fn main() -> std::io::Result<()> {
    // The board runs it with no arguments; `c-join <data-dir> <out-dir>` is for local runs.
    let arg = |n, d: &str| env::args().nth(n).unwrap_or(d.into());
    let (data, out_dir) = (arg(1, "/data"), arg(2, "/out"));
    // SD_WORKERS is rule 30's test-only override (the hash runs force 1, 4, 12, 24); the board never
    // sets it, so the scored run uses 12, the scoring box's hardware threads.
    let workers = env::var("SD_WORKERS").map_or(12, |v| v.parse().unwrap());

    // regions.csv is parsed by the workers together at their start (regions::Build).
    let regions = regions::Build::new(map_padded(&format!("{data}/regions.csv")));
    let recs = map_padded(&format!("{data}/records.csv"));
    let (totals, distinct) = pass2::run(recs, workers, &regions);
    let keys = regions.keys();
    // One line per group with records, sorted by key bytewise (c-join.md:127-129): the keys are
    // packed big-endian (regions.rs), with `_UNMATCHED`'s (`_UN`) above every code, so u32 order is
    // bytewise.
    let mut lines: Vec<_> = (0..keys.len()).filter(|&g| totals[g].count > 0).collect();
    lines.sort_unstable_by_key(|&g| keys[g]);
    let mut out = Vec::new();
    for g in lines {
        let t = &totals[g];
        out.extend_from_slice(&keys[g].to_be_bytes()[1..]);
        // Group 0 is `_UNMATCHED`: its packed key prints the `_UN` above, so append the rest.
        if g == 0 {
            out.extend_from_slice(b"MATCHED");
        }
        write!(out, " {} {} ", t.count, t.id_sum)?;
        for cents in [t.amount_sum, t.amount_min, t.amount_max] {
            write!(out, "{} ", money(cents))?;
        }
        writeln!(out, "{} {} {}", distinct[g], t.name_bytes, t.note_bytes)?;
    }
    std::fs::write(format!("{out_dir}/aggregates.txt"), &out)?;
    // Exit here instead of returning, which would free every worker's slots and the region table
    // one by one before the kernel tears the process down anyway (c-37 timeline probe).
    std::process::exit(0);
}
