//! regions.csv: `region_id,country` lines, LF-terminated, ids unique and ascending, country `[A-Z]{3}`
//! (c-join.md:58-59). Parsed into a dense table region id -> group index; group 0 is `_UNMATCHED`.

/// Region id -> group index. One byte per region while the file names at most 255 countries,
/// two bytes beyond that.
pub enum RegionTable {
    Narrow(Vec<u8>),
    Wide(Vec<u16>),
}

pub trait Lookup {
    /// Group of a record's region id; ids the table does not hold (holes, or above its range) are group 0.
    fn group(&self, region_id: usize) -> usize;
}

impl Lookup for [u8] {
    fn group(&self, region_id: usize) -> usize {
        self.get(region_id).map_or(0, |&g| g as usize)
    }
}

impl Lookup for [u16] {
    fn group(&self, region_id: usize) -> usize {
        self.get(region_id).map_or(0, |&g| g as usize)
    }
}

/// The parsed lookup: the table plus each group's output key (index 0 is `_UNMATCHED`).
pub struct Regions {
    pub table: RegionTable,
    pub keys: Vec<Vec<u8>>,
}

fn parse_uint(digits: &[u8]) -> usize {
    digits.iter().fold(0, |n, &d| n * 10 + (d - b'0') as usize)
}

/// `region_id,country` -> (region id, country code bytes).
fn split(line: &[u8]) -> (usize, &[u8]) {
    let comma = line.iter().position(|&b| b == b',').expect("regions.csv: line without a comma");
    (parse_uint(&line[..comma]), &line[comma + 1..])
}

pub fn parse(bytes: &[u8]) -> Regions {
    let all_lines = || bytes.split(|&b| b == b'\n').filter(|l| !l.is_empty());
    // Ids are ascending (c-join.md:59), so the last line holds the largest and sizes the table exactly.
    let max_id = all_lines().next_back().filter(|l| l[0].is_ascii_digit()).map_or(0, |l| split(l).0);

    // Country code [A-Z]{3} (c-join.md:59) -> group index, through a dense 26^3 table.
    let mut code_group = vec![0u16; 26 * 26 * 26];
    let mut keys: Vec<Vec<u8>> = vec![b"_UNMATCHED".to_vec()];
    let mut table = RegionTable::Narrow(vec![0u8; max_id + 1]);
    for line in all_lines().skip(1) { // skip the header `region_id,country`
        let (id, code) = split(line);
        assert!(code.len() == 3 && code.iter().all(u8::is_ascii_uppercase), "regions.csv: bad country code");
        let slot = &mut code_group[code.iter().fold(0, |n, &c| n * 26 + (c - b'A') as usize)];
        if *slot == 0 {
            *slot = keys.len() as u16;
            keys.push(code.to_vec());
        }
        let group = *slot;
        if let RegionTable::Narrow(narrow) = &table && group > u8::MAX as u16 {
            table = RegionTable::Wide(narrow.iter().map(|&g| g as u16).collect());
        }
        match &mut table {
            RegionTable::Narrow(t) => t[id] = group as u8,
            RegionTable::Wide(t) => t[id] = group,
        }
    }
    Regions { table, keys }
}
