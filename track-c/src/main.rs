//! Track C (join): per-country aggregates of records.csv joined against regions.csv, as specified
//! in specs/c-join.md, parsed by N workers over N byte ranges.

mod cuts;
mod mmap;
mod names;
mod pass2;
mod records;
mod regions;

use std::io::Write;
use std::time::{Duration, Instant};

use names::Intern;
use records::Slot;
use regions::RegionTable;

/// Worker count: SD_WORKERS is rule 30's test-only override (the hash runs force 1, 4, 12, 24);
/// the board never sets it, so the scored run uses 12, the scoring box's hardware threads.
fn worker_count() -> usize {
    std::env::var("SD_WORKERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(12)
}

/// Integer cents as `-?[0-9]+\.[0-9]{2}`, never `-0.00` (c-join.md:131).
fn push_money(out: &mut Vec<u8>, cents: i64) {
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.unsigned_abs();
    write!(out, "{sign}{}.{:02}", abs / 100, abs % 100).unwrap();
}

/// Starting slots per intern shard (256 shards, so 16K slots, 128 KiB, before any growth).
/// SD_INTERN_SLOTS is a test-only override: forcing a tiny table (e.g. 2) runs the growth path
/// on every shard and must give the same hash. The board never sets it.
fn intern_slots() -> usize {
    std::env::var("SD_INTERN_SLOTS").ok().and_then(|v| v.parse().ok()).unwrap_or(64)
}

fn main() {
    // The board runs it with no arguments; `c-join <data-dir> <out-dir>` is for local runs.
    let mut args = std::env::args().skip(1);
    let data = args.next().unwrap_or_else(|| "/data".into());
    let out_dir = args.next().unwrap_or_else(|| "/out".into());
    let workers = worker_count();

    let start = Instant::now();
    let regions = regions::parse(mmap::map_file(&format!("{data}/regions.csv")));
    let groups = regions.keys.len();
    let t_regions = start.elapsed();

    let b = mmap::map_file(&format!("{data}/records.csv"));
    // Name offsets are stored as u32.
    assert!(b.len() < u32::MAX as usize, "records.csv over 4 GiB");
    let first = records::header_end(b);
    let starts = cuts::record_starts(b, first, workers);
    let t_cuts = start.elapsed();

    let run = |starts: &[usize], intern: &Intern| match &regions.table {
        RegionTable::Narrow(t) => pass2::run(b, starts, t.as_slice(), groups, intern),
        RegionTable::Wide(t) => pass2::run(b, starts, t.as_slice(), groups, intern),
    };
    let mut intern = Intern::new(intern_slots());
    let mut pass2 = run(&starts, &intern);
    if !pass2.exact {
        // e1: a cut was not a record start, so the parse is untrustworthy and the intern holds
        // names from wrong records; rerun on one worker over the whole file, where the start
        // (`first`) is always right.
        intern = Intern::new(intern_slots());
        pass2 = run(&[first, b.len()], &intern);
    }
    let t_parse = start.elapsed();

    let mut totals = vec![Slot::default(); groups];
    for slots in &pass2.slots {
        for (total, slot) in totals.iter_mut().zip(slots) {
            total.merge(slot);
        }
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
        out.push(b' ');
        push_money(&mut out, t.amount_min);
        out.push(b' ');
        push_money(&mut out, t.amount_max);
        writeln!(out, " {} {} {}", pass2.distinct[g], t.name_bytes, t.note_bytes).unwrap();
    }
    let t_format = start.elapsed();

    std::fs::write(format!("{out_dir}/aggregates.txt"), &out).expect("write aggregates.txt");
    let t_write = start.elapsed();

    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    let per_thread: Vec<String> = pass2.times.iter().map(|&t| format!("{:.1}", ms(t))).collect();
    eprintln!(
        "phases ms: regions {:.1} pass1 {:.1} pass2+names {:.1} merge+sort+format {:.1} write {:.1} | records {} | names {} | rounds {} | workers {workers} | pass2 per thread ms: {}",
        ms(t_regions),
        ms(t_cuts - t_regions),
        ms(t_parse - t_cuts),
        ms(t_format - t_parse),
        ms(t_write - t_format),
        pass2.records,
        intern.count(),
        pass2.rounds,
        per_thread.join(" "),
    );
}
