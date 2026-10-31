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
    fn group(&self, region_id: u64) -> usize;
}

/// The u8 and u16 tables share one impl: a group index fits either element type. The table's last
/// entry (one past the largest region id) is group 0, and every id above the range is clamped to it:
/// a `min` (one cmov) instead of a bounds check and its branch.
impl<T: Copy + Into<usize>> Lookup for Vec<T> {
    #[inline(always)]
    fn group(&self, region_id: u64) -> usize {
        // SAFETY: the table is never empty (`parse` sizes it largest id + 2), so the clamp is in bounds.
        unsafe { (*self.get_unchecked((region_id as usize).min(self.len() - 1))).into() }
    }
}

/// The parsed lookup: the table plus each group's output key (index 0 is `_UNMATCHED`).
pub struct Regions {
    pub table: RegionTable,
    pub keys: Vec<Vec<u8>>,
}

/// `region_id,country` -> (region id, country code bytes).
fn split(line: &[u8]) -> (usize, &[u8]) {
    let comma = line.iter().position(|&c| c == b',').expect("regions.csv: no comma");
    let id = line[..comma].iter().fold(0, |n, &d| n * 10 + (d - b'0') as usize);
    (id, &line[comma + 1..])
}

pub fn parse(bytes: &[u8]) -> Regions {
    let lines = || bytes.split(|&b| b == b'\n').filter(|l| !l.is_empty());
    // Ids are ascending (c-join.md:59), so the last line holds the largest and sizes the table exactly.
    let last = lines().next_back().filter(|l| l[0].is_ascii_digit());
    let max_id = last.map_or(0, |l| split(l).0);

    // Country code [A-Z]{3} (c-join.md:59) -> group index, through a dense 26^3 table.
    let mut code_group = vec![0u16; 26 * 26 * 26];
    let mut keys: Vec<Vec<u8>> = vec![b"_UNMATCHED".to_vec()];
    // Ids 0..=max_id, plus the group-0 entry every larger id is clamped to (`Lookup::group`).
    let mut table = RegionTable::Narrow(vec![0u8; max_id + 2]);
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
