//! Track C (join): per-country aggregates of records.csv joined against regions.csv, as specified
//! in specs/c-join.md. This build is single-threaded.

mod mmap;
mod names;
mod records;
mod regions;

use std::io::Write;
use std::time::Instant;

use names::NameIds;
use records::Group;
use regions::RegionTable;

/// Worker count: SD_WORKERS is rule 30's test-only override (the hash runs force 1, 4, 12, 24);
/// the board never sets it. This build ignores it and always parses on one thread.
fn worker_count() -> usize {
    std::env::var("SD_WORKERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1)
}

/// Integer cents as `-?[0-9]+\.[0-9]{2}`, never `-0.00` (c-join.md:131).
fn push_money(out: &mut Vec<u8>, cents: i64) {
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.unsigned_abs();
    write!(out, "{sign}{}.{:02}", abs / 100, abs % 100).unwrap();
}

fn main() {
    // The board runs it with no arguments; `c-join <data-dir> <out-dir>` is for local runs.
    let mut args = std::env::args().skip(1);
    let data = args.next().unwrap_or_else(|| "/data".into());
    let out_dir = args.next().unwrap_or_else(|| "/out".into());

    let start = Instant::now();
    let regions = regions::parse(mmap::map_file(&format!("{data}/regions.csv")));
    let t_regions = start.elapsed();

    let records_csv = mmap::map_file(&format!("{data}/records.csv"));
    let mut groups: Vec<Group> = regions.keys.iter().map(|_| Group::default()).collect();
    let mut names = NameIds::default();
    let record_count = match &regions.table {
        RegionTable::Narrow(t) => records::parse(records_csv, t.as_slice(), &mut groups, &mut names),
        RegionTable::Wide(t) => records::parse(records_csv, t.as_slice(), &mut groups, &mut names),
    };
    let t_records = start.elapsed();

    // One line per group with records, sorted by key bytewise (c-join.md:127-129).
    let mut lines: Vec<(&[u8], &Group)> =
        regions.keys.iter().map(Vec::as_slice).zip(&groups).filter(|(_, g)| g.count > 0).collect();
    lines.sort_unstable_by_key(|&(key, _)| key);
    let mut out = Vec::with_capacity(lines.len() * 96);
    for (key, g) in lines {
        out.extend_from_slice(key);
        write!(out, " {} {} ", g.count, g.id_sum).unwrap();
        push_money(&mut out, g.amount_sum);
        out.push(b' ');
        push_money(&mut out, g.amount_min);
        out.push(b' ');
        push_money(&mut out, g.amount_max);
        writeln!(out, " {} {} {}", g.names.len(), g.name_bytes, g.note_bytes).unwrap();
    }
    let t_format = start.elapsed();

    std::fs::write(format!("{out_dir}/aggregates.txt"), &out).expect("write aggregates.txt");
    let t_write = start.elapsed();

    let ms = |d: std::time::Duration| d.as_secs_f64() * 1e3;
    eprintln!(
        "phases ms: regions {:.1} records {:.1} sort+format {:.1} write {:.1} | records {record_count} | workers 1 (SD_WORKERS={})",
        ms(t_regions),
        ms(t_records - t_regions),
        ms(t_format - t_records),
        ms(t_write - t_format),
        worker_count(),
    );
}
