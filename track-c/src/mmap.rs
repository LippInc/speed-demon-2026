//! Read-only file mappings through libc's mmap (std links libc; no crate needed).

use std::{ffi::c_void, fs::File, os::fd::AsRawFd};

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
        // prot 1 = PROT_READ, flags 2 = MAP_PRIVATE (mmap(2)); -1 (all bits set) is MAP_FAILED.
        let ptr = mmap(std::ptr::null_mut(), len, 1, 2, file.as_raw_fd(), 0);
        assert!(ptr as usize != usize::MAX, "mmap {path} failed");
        std::slice::from_raw_parts(ptr as *const u8, len)
    }
}
