//! The budget for lopdf on a large file (plan M0: "lopdf parse time and peak memory on the big
//! fixture are recorded and a budget is set").
//!
//! lopdf reads the whole file into memory and copies every stream's data into its object
//! graph, so loading costs about twice the file size in memory, and a verified incremental
//! save about three times (the loaded state, the file read back, and its re-parse). That is
//! why the plan parses lazily, on the first edit, and never at open. The constants below put
//! a ceiling on it. `tests/budget.rs` asserts them on a scan set generated on the spot; the
//! figures measured on the full 300 MB set are in docs/measurements.md.
//!
//! Measured on 2026-11-24 (release build, 38 Letter pages of 2550 x 3300 px noise, 305 MiB):
//! read 63 ms, parse 14 ms, peak working set 616 MiB after the load (2.0 x the file) and
//! 922 MiB after a verified incremental save (3.0 x), which took 324 ms.

use std::time::Duration;

const MIB: u64 = 1024 * 1024;

/// Memory allowed beyond the per-byte factors: the object graph of a file with few bytes but
/// many objects, and the allocator's own slack.
pub const PEAK_BASE_BYTES: u64 = 32 * MIB;

/// Peak heap while a file is loaded, per byte of file (measured: 2.0).
pub const LOAD_PEAK_PER_FILE_BYTE: f64 = 2.5;

/// Peak heap during a verified incremental save, per byte of file (measured: 3.0).
pub const SAVE_PEAK_PER_FILE_BYTE: f64 = 3.5;

/// Time to read and parse, per 100 MiB of file (measured: 25 ms), on top of
/// [`TIME_BASE`].
pub const LOAD_TIME_PER_100_MIB: Duration = Duration::from_millis(500);

/// Time for a verified incremental save, per 100 MiB of file (measured: 106 ms), on top of
/// [`TIME_BASE`].
pub const SAVE_TIME_PER_100_MIB: Duration = Duration::from_millis(1500);

/// Fixed part of both time budgets.
pub const TIME_BASE: Duration = Duration::from_millis(500);

/// The most heap a load of a `file_len`-byte file may hold at once.
pub fn load_peak_bytes(file_len: u64) -> u64 {
    PEAK_BASE_BYTES + (file_len as f64 * LOAD_PEAK_PER_FILE_BYTE) as u64
}

/// The most heap a verified incremental save of a `file_len`-byte file may hold at once.
pub fn save_peak_bytes(file_len: u64) -> u64 {
    PEAK_BASE_BYTES + (file_len as f64 * SAVE_PEAK_PER_FILE_BYTE) as u64
}

fn scaled(per_100_mib: Duration, file_len: u64) -> Duration {
    TIME_BASE + per_100_mib.mul_f64(file_len as f64 / (100 * MIB) as f64)
}

/// The longest a read plus parse of a `file_len`-byte file may take.
pub fn load_time(file_len: u64) -> Duration {
    scaled(LOAD_TIME_PER_100_MIB, file_len)
}

/// The longest a verified incremental save of a `file_len`-byte file may take.
pub fn save_time(file_len: u64) -> Duration {
    scaled(SAVE_TIME_PER_100_MIB, file_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_scales_with_the_file_and_covers_what_was_measured() {
        // The 300 MB scan set.
        let set = 319_789_952u64;
        // Measured peaks (working set, which includes the heap): 616 and 922 MiB.
        assert!(load_peak_bytes(set) > 616 * MIB && load_peak_bytes(set) < 900 * MIB);
        assert!(save_peak_bytes(set) > 922 * MIB && save_peak_bytes(set) < 1300 * MIB);
        // Measured times: 76 ms and 324 ms.
        assert!(load_time(set) > Duration::from_millis(76) * 5);
        assert!(save_time(set) > Duration::from_millis(324) * 5);
        assert!(load_time(set) < Duration::from_secs(3));
        assert!(save_time(set) < Duration::from_secs(6));
        assert_eq!(load_time(0), TIME_BASE);
        assert_eq!(load_peak_bytes(0), PEAK_BASE_BYTES);
        assert!(load_peak_bytes(2 * set) > load_peak_bytes(set));
    }
}
