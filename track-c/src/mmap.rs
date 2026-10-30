//! Read-only file mappings through libc's mmap (std links libc; no crate needed).

use std::ffi::c_void;
use std::fs::File;
use std::os::fd::AsRawFd;

const PROT_READ: i32 = 0x1;
const MAP_PRIVATE: i32 = 0x02;
const MAP_FAILED: *mut c_void = !0 as *mut c_void;

unsafe extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut c_void;
}

/// Maps a whole file read-only; pages fault in as they are touched (probe P2: no MAP_POPULATE).
/// The mapping lives until exit, so it is never unmapped.
pub fn map_file(path: &str) -> &'static [u8] {
    let file = File::open(path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let len = file.metadata().expect("stat").len() as usize;
    if len == 0 {
        return &[];
    }
    // SAFETY: a fresh read-only private mapping of an input file nobody writes during the run.
    unsafe {
        let ptr = mmap(std::ptr::null_mut(), len, PROT_READ, MAP_PRIVATE, file.as_raw_fd(), 0);
        assert!(ptr != MAP_FAILED, "mmap {path} failed");
        std::slice::from_raw_parts(ptr as *const u8, len)
    }
}
