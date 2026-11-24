//! pl-text: text recognition behind a trait (see docs/plan.md sections 4, 6 and 8).
//!
//! What is here is the M0 part of the plan's `pl-text`: the [`OcrEngine`] trait and the first
//! engine behind it, [`WindowsOcr`] (the `Windows.Media.Ocr` engine that ships with Windows).
//! The searchable text layer, redaction and flatten that the plan also places in this crate
//! arrive with their milestones (M11, M13).
//!
//! # Why a trait
//!
//! Microsoft documents `Windows.Media.Ocr` as supported only for apps with package identity.
//! Plumbline ships as a plain, unpackaged exe, so the engine may be refused, and it depends on
//! the language packs installed on the user's PC. Every caller therefore goes through
//! [`OcrEngine`], and an engine that cannot be used says so with a typed [`OcrError`] instead
//! of failing the feature. A bundled engine (`ocrs`, or Tesseract) can be put behind the same
//! trait without touching a caller. The outcome of the M0 spike on the development machine is
//! recorded in docs/measurements.md.
//!
//! # Coordinates
//!
//! An engine works on a raster, so its boxes are in image pixels: origin at the top-left
//! corner, y down. Mapping them to page space is the caller's job, with the `pl-geom`
//! transform of the render the image came from.

mod ocr;
#[cfg(windows)]
mod windows_ocr;

pub use ocr::{OcrEngine, OcrError, OcrImage, OcrLine, OcrPage, OcrWord, PixelRect};
#[cfg(windows)]
pub use windows_ocr::WindowsOcr;
