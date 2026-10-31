//! regions.csv: `region_id,country` lines, LF-terminated, ids unique and ascending, country `[A-Z]{3}`
//! (c-join.md:58-59). Parsed into a dense table region id -> group index; group 0 is `_UNMATCHED`.

use crate::records::uint_until;

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

/// The u8 and u16 tables share one impl: a group index fits either element type, and `get` is
/// None past the table's end, which is the group-0 default.
impl<T: Copy + Into<usize>> Lookup for Vec<T> {
    fn group(&self, region_id: usize) -> usize {
        self.get(region_id).map_or(0, |&g| g.into())
    }
}

/// The parsed lookup: the table plus each group's output key (index 0 is `_UNMATCHED`).
pub struct Regions {
    pub table: RegionTable,
    pub keys: Vec<Vec<u8>>,
}

/// `region_id,country` -> (region id, country code bytes).
fn split(line: &[u8]) -> (usize, &[u8]) {
    let (id, country) = uint_until(line, 0, b',');
    (id as usize, &line[country..])
}

pub fn parse(bytes: &[u8]) -> Regions {
    let lines = || bytes.split(|&b| b == b'\n').filter(|l| !l.is_empty());
    // Ids are ascending (c-join.md:59), so the last line holds the largest and sizes the table exactly.
    let last = lines().next_back().filter(|l| l[0].is_ascii_digit());
    let max_id = last.map_or(0, |l| split(l).0);

    // Country code [A-Z]{3} (c-join.md:59) -> group index, through a dense 26^3 table.
    let mut code_group = vec![0u16; 26 * 26 * 26];
    let mut keys: Vec<Vec<u8>> = vec![b"_UNMATCHED".to_vec()];
    let mut table = RegionTable::Narrow(vec![0u8; max_id + 1]);
    for line in lines().skip(1) { // skip the header `region_id,country`
        let (id, code) = split(line);
        let valid = code.len() == 3 && code.iter().all(u8::is_ascii_uppercase);
        assert!(valid, "regions.csv: bad country code");
        let slot = &mut code_group[code.iter().fold(0, |n, &c| n * 26 + (c - b'A') as usize)];
        if *slot == 0 {
            *slot = keys.len() as u16;
            keys.push(code.to_vec());
        }
        if let RegionTable::Narrow(narrow) = &table && *slot > u8::MAX as u16 {
            table = RegionTable::Wide(narrow.iter().map(|&g| g as u16).collect());
        }
        match &mut table {
            RegionTable::Narrow(t) => t[id] = *slot as u8,
            RegionTable::Wide(t) => t[id] = *slot,
        }
    }
    Regions { table, keys }
}
