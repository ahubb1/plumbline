//! The render-time budget for a dense sheet, and with it the decision about the worker pool.
//!
//! The plan (docs/plan.md, M1) gates the `render-worker.exe` process pool on one measurement:
//! a 200,000-segment sheet at 400% zoom in a 1920 x 1080 viewport must meet a budget on the
//! single PDFium thread, or the pool gets built. These constants are that budget. The test
//! `dense_sheet_at_400_percent_meets_the_budget` (tests/bench_dense.rs) asserts them on every
//! `cargo test`; the measured figures and how they were taken are in docs/measurements.md.
//!
//! Measured on 2026-11-24 (release build, PDFium 8125, the pl-testkit dense sheet): first
//! tile on screen 95 to 105 ms after the open began, of which about 100 ms is PDFium parsing
//! the page once; the whole viewport 180 to 190 ms (35 tiles) at 100% display scaling and
//! about 295 ms (104 tiles) at 200%. Each budget below is roughly five times its measurement,
//! so the gate trips on a real regression, not on a busy machine.
//!
//! Outcome: the budget is met with a wide margin, so the worker pool is **not** built.

use std::time::Duration;

/// Segments on the sheet the budget is stated for.
pub const DENSE_SHEET_SEGMENTS: usize = 200_000;

/// Zoom the budget is stated for (400%).
pub const DENSE_SHEET_ZOOM: f64 = 4.0;

/// The viewport the budget is stated for, in logical pixels.
pub const DENSE_SHEET_VIEWPORT: [f64; 2] = [1920.0, 1080.0];

/// From starting to open the file to the first visible tile being ready (the page content is
/// parsed once inside this).
pub const DENSE_FIRST_TILE: Duration = Duration::from_millis(500);

/// From starting to open the file to every visible tile being ready, at 100% display scaling
/// (one device pixel per logical pixel).
pub const DENSE_VIEWPORT: Duration = Duration::from_millis(1000);

/// As [`DENSE_VIEWPORT`] at 200% display scaling (four times the pixels).
pub const DENSE_VIEWPORT_HIDPI: Duration = Duration::from_millis(1500);
