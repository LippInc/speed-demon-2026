//! Track C (join): per-country aggregates of records.csv joined against regions.csv, as specified
//! in specs/c-join.md, parsed by N workers over N byte ranges.

mod names;
mod pass2;
mod records;
mod regions;

use std::{fs::File, io::Write, os::fd::AsRawFd};

use crate::{records::BOM, records::GUARD, records::Slot};

/// A test-only integer env override (SD_WORKERS): set to a number, else absent.
fn env_num(name: &str) -> Option<usize> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

/// Integer cents as `-?[0-9]+\.[0-9]{2}`, never `-0.00` (c-join.md:131), plus the trailing
/// separator of every money field on an output line.
fn push_money(out: &mut Vec<u8>, cents: i64) {
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.unsigned_abs();
    write!(out, "{sign}{}.{:02} ", abs / 100, abs % 100).unwrap();
}

// Read-only file mappings through libc's mmap (std links libc; no crate needed).
unsafe extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
}

/// Maps a whole file (records.csv, regions.csv) followed by GUARD bytes repeating `"` `,` LF
/// (records.rs), and returns the mapping and the file's length. The mapping lives until exit, so it
/// is never unmapped. A private anonymous reservation of the file's length plus 4 KiB is
/// mapped first, then the file over its start (MAP_FIXED), so the bytes after the file are the file's
/// last page tail or the reservation, both private and zero; the guard is written there (at most one
/// page copied, 4 KiB of M). Every scan then stops at the guard, so the parse needs no bounds checks.
fn map_padded(path: &str) -> (&'static [u8], usize) {
    let file = File::open(path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let len = file.metadata().expect("stat").len() as usize;
    // SAFETY: fresh private mappings: the reservation, then the file over its first `len` bytes.
    unsafe {
        // prot 3 = PROT_READ | PROT_WRITE; flags 2 = MAP_PRIVATE, 0x20 = MAP_ANONYMOUS, 0x10 = MAP_FIXED.
        let base = mmap(std::ptr::null_mut(), len + 4096, 3, 2 | 0x20, -1, 0);
        assert!(base as usize != usize::MAX, "mmap reservation failed");
        if len != 0 {
            let at = mmap(base, len, 3, 2 | 0x10, file.as_raw_fd(), 0);
            assert!(at == base, "mmap {path} failed");
        }
        for k in 0..GUARD {
            *base.add(len + k) = b"\",\n"[k % 3];
        }
        (std::slice::from_raw_parts(base, len + GUARD), len)
    }
}

fn main() {
    // The board runs it with no arguments; `c-join <data-dir> <out-dir>` is for local runs.
    let mut args = std::env::args().skip(1);
    let data = args.next().unwrap_or_else(|| "/data".into());
    let out_dir = args.next().unwrap_or_else(|| "/out".into());
    // Worker count: SD_WORKERS is rule 30's test-only override (the hash runs force 1, 4, 12, 24);
    // the board never sets it, so the scored run uses 12, the scoring box's hardware threads.
    let workers = env_num("SD_WORKERS").filter(|&n| n > 0).unwrap_or(12);

    // regions.csv was parsed by one thread before any worker started: about 4.2 ms on box 2, 79 % of
    // it user code (c-47 F0 perf). The workers now parse it together at their start (regions::Build).
    let (rbuf, rlen) = map_padded(&format!("{data}/regions.csv"));
    let regions = regions::Build::new(&rbuf[..rlen]);

    let (b, len) = map_padded(&format!("{data}/records.csv"));
    // Index just past the header: a BOM may start the file, and the header holds no quotes and ends
    // at the first LF (c-join.md:18-19, 43).
    let file = &b[..len];
    let bom = if file.starts_with(BOM) { BOM.len() } else { 0 };
    let lf = file[bom..].iter().position(|&c| c == b'\n');
    let first = lf.map_or(len, |p| p + bom + 1);
    let mut pass2 = pass2::run(b, len, first, pass2::CHUNK, workers, &regions);
    if !pass2.exact {
        // e1: a cut was not a record start, so the parse is untrustworthy and the owners hold names
        // from wrong records; rerun on one worker over the whole file, where the start (`first`) is
        // always right.
        pass2 = pass2::run(b, len, first, len, 1, &regions);
    }

    let keys = regions.keys();
    let groups = keys.len();
    let mut totals = vec![Slot::new(); groups];
    for slots in &pass2.slots {
        for (total, slot) in totals.iter_mut().zip(slots) {
            total.merge(slot);
        }
    }
    // One line per group with records, sorted by key bytewise (c-join.md:127-129).
    let mut lines: Vec<usize> = (0..groups).filter(|&g| totals[g].count > 0).collect();
    lines.sort_unstable_by_key(|&g| &keys[g]);
    let mut out = Vec::with_capacity(lines.len() * 96);
    for g in lines {
        let t = &totals[g];
        out.extend_from_slice(&keys[g]);
        write!(out, " {} {} ", t.count, t.id_sum).unwrap();
        push_money(&mut out, t.amount_sum);
        push_money(&mut out, t.amount_min);
        push_money(&mut out, t.amount_max);
        write!(out, "{} {} ", pass2.distinct[g], t.name_bytes).unwrap();
        writeln!(out, "{}", t.note_bytes).unwrap();
    }

    std::fs::write(format!("{out_dir}/aggregates.txt"), &out).expect("write aggregates.txt");
    // Exit here instead of returning, which would free every worker's slots and the region table
    // one by one before the kernel tears the process down anyway (c-37 timeline probe).
    std::process::exit(0);
}
