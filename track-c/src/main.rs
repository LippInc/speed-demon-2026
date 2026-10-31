//! Track C (join): per-country aggregates of records.csv joined against regions.csv, as specified
//! in specs/c-join.md, parsed by N workers over N byte ranges.

mod names;
mod pass2;
mod records;
mod regions;

use std::{ffi::c_void, fs::File, io::Write, os::fd::AsRawFd};

use crate::{names::Intern, records::BOM, records::Slot, regions::RegionTable};

/// A test-only integer env override (SD_WORKERS, SD_INTERN_SLOTS): set to a number, else absent.
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
    fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut c_void;
}

/// Maps a whole file read-only; pages fault in as they are touched (probe P2: no MAP_POPULATE).
/// The mapping lives until exit, so it is never unmapped.
fn map_file(path: &str) -> &'static [u8] {
    let file = File::open(path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let len = file.metadata().expect("stat").len() as usize;
    // SAFETY: a fresh read-only private mapping of an input file nobody writes during the run.
    let map = (len != 0).then(|| unsafe {
        // prot 1 = PROT_READ, flags 2 = MAP_PRIVATE (mmap(2)); -1 (all bits set) is MAP_FAILED.
        let ptr = mmap(std::ptr::null_mut(), len, 1, 2, file.as_raw_fd(), 0);
        assert!(ptr as usize != usize::MAX, "mmap {path} failed");
        std::slice::from_raw_parts(ptr as *const u8, len)
    });
    map.unwrap_or(&[])
}

fn main() {
    // The board runs it with no arguments; `c-join <data-dir> <out-dir>` is for local runs.
    let mut args = std::env::args().skip(1);
    let data = args.next().unwrap_or_else(|| "/data".into());
    let out_dir = args.next().unwrap_or_else(|| "/out".into());
    // Worker count: SD_WORKERS is rule 30's test-only override (the hash runs force 1, 4, 12, 24);
    // the board never sets it, so the scored run uses 12, the scoring box's hardware threads.
    let workers = env_num("SD_WORKERS").filter(|&n| n > 0).unwrap_or(12);

    let regions = regions::parse(map_file(&format!("{data}/regions.csv")));
    let groups = regions.keys.len();

    let b = map_file(&format!("{data}/records.csv"));
    // Name offsets are stored as u32.
    assert!(b.len() < u32::MAX as usize, "records.csv over 4 GiB");
    // Index just past the header: a BOM may start the file, and the header holds no quotes and ends
    // at the first LF (c-join.md:18-19, 43).
    let bom = if b.starts_with(BOM) { BOM.len() } else { 0 };
    let lf = b[bom..].iter().position(|&c| c == b'\n');
    let first = lf.map_or(b.len(), |p| p + bom + 1);
    let starts = records::record_starts(b, first, workers);

    let run = |starts: &[usize], intern: &Intern| match &regions.table {
        RegionTable::Narrow(t) => pass2::run(b, starts, t.as_slice(), groups, intern),
        RegionTable::Wide(t) => pass2::run(b, starts, t.as_slice(), groups, intern),
    };
    // Starting slots per intern shard (256 shards, so 16K slots, 128 KiB, before any growth).
    // SD_INTERN_SLOTS is a test-only override: forcing a tiny table (e.g. 2) runs the growth path
    // on every shard and must give the same hash. The board never sets it.
    let mut intern = Intern::new(env_num("SD_INTERN_SLOTS").unwrap_or(64));
    let mut pass2 = run(&starts, &intern);
    if !pass2.exact {
        // e1: a cut was not a record start, so the parse is untrustworthy and the intern holds
        // names from wrong records; rerun on one worker over the whole file, where the start
        // (`first`) is always right.
        intern = Intern::new(env_num("SD_INTERN_SLOTS").unwrap_or(64));
        pass2 = run(&[first, b.len()], &intern);
    }

    let mut totals = vec![Slot::default(); groups];
    for slots in &pass2.slots {
        totals.iter_mut().zip(slots).for_each(|(total, slot)| total.merge(slot));
    }
    // One line per group with records, sorted by key bytewise (c-join.md:127-129).
    let mut lines: Vec<usize> = (0..groups).filter(|&g| totals[g].count > 0).collect();
    lines.sort_unstable_by_key(|&g| &regions.keys[g]);
    let mut out = Vec::with_capacity(lines.len() * 96);
    for g in lines {
        let t = &totals[g];
        out.extend_from_slice(&regions.keys[g]);
        write!(out, " {} {} ", t.count, t.id_sum).unwrap();
        push_money(&mut out, t.amount_sum);
        push_money(&mut out, t.amount_min);
        push_money(&mut out, t.amount_max);
        write!(out, "{} {} ", pass2.distinct[g], t.name_bytes).unwrap();
        writeln!(out, "{}", t.note_bytes).unwrap();
    }

    std::fs::write(format!("{out_dir}/aggregates.txt"), &out).expect("write aggregates.txt");
}
