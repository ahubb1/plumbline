//! [`WindowsOcr`]: the `Windows.Media.Ocr` engine behind [`OcrEngine`].
//!
//! Nothing here shows a window or a prompt: the engine is an in-process WinRT component that
//! works on a bitmap in memory.

use windows::core::Error;
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine as WinOcrEngine;
use windows::Storage::Streams::{DataWriter, IBuffer};

use crate::ocr::{OcrEngine, OcrError, OcrImage, OcrLine, OcrPage, OcrWord};

/// The `E_ACCESSDENIED` and "class not registered" results an engine that is withheld from
/// this process would fail with.
const E_ACCESSDENIED: i32 = 0x8007_0005_u32 as i32;
const REGDB_E_CLASSNOTREG: i32 = 0x8004_0154_u32 as i32;

fn describe(error: &Error) -> String {
    format!("{} (0x{:08X})", error.message(), error.code().0 as u32)
}

/// An error from creating or probing the engine: it cannot be used here.
fn unavailable(error: Error) -> OcrError {
    OcrError::Unavailable(describe(&error))
}

/// An error from a call on a working engine.
fn failed(error: Error) -> OcrError {
    match error.code().0 {
        E_ACCESSDENIED | REGDB_E_CLASSNOTREG => OcrError::Unavailable(describe(&error)),
        _ => OcrError::Failed(describe(&error)),
    }
}

/// The text recogniser built into Windows, using the user's profile languages.
///
/// Microsoft documents this API as supported for packaged apps only. [`WindowsOcr::new`] is
/// where an unpackaged process finds out whether it is given the engine anyway.
pub struct WindowsOcr {
    engine: WinOcrEngine,
    language: Option<String>,
    max_side: u32,
}

impl WindowsOcr {
    /// Creates the engine for the first of the user's languages that has an OCR language
    /// pack. [`OcrError::NoLanguage`] when none has one; [`OcrError::Unavailable`] when the
    /// engine cannot be created in this process.
    pub fn new() -> Result<WindowsOcr, OcrError> {
        let max_side = WinOcrEngine::MaxImageDimension().map_err(unavailable)?;
        // The call "succeeds" with a null engine when no profile language can be recognised;
        // the binding reports that as an error with a success code.
        let engine = match WinOcrEngine::TryCreateFromUserProfileLanguages() {
            Ok(engine) => engine,
            Err(error) if error.code().is_ok() => return Err(OcrError::NoLanguage),
            Err(error) => return Err(unavailable(error)),
        };
        let language = engine
            .RecognizerLanguage()
            .and_then(|l| l.LanguageTag())
            .ok()
            .map(|tag| tag.to_string());
        Ok(WindowsOcr {
            engine,
            language,
            max_side,
        })
    }

    /// The BCP-47 tags of the OCR languages installed on this machine.
    pub fn installed_languages() -> Result<Vec<String>, OcrError> {
        let languages = WinOcrEngine::AvailableRecognizerLanguages().map_err(unavailable)?;
        let mut tags = Vec::new();
        for language in languages {
            tags.push(language.LanguageTag().map_err(unavailable)?.to_string());
        }
        Ok(tags)
    }
}

/// The image as the BGRA8 bitmap the engine takes.
fn to_bitmap(image: &OcrImage<'_>) -> Result<SoftwareBitmap, Error> {
    let mut bgra = image.rgba.to_vec();
    for px in bgra.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
        px[3] = 255;
    }
    let writer = DataWriter::new()?;
    writer.WriteBytes(&bgra)?;
    let buffer: IBuffer = writer.DetachBuffer()?;
    SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
        &buffer,
        BitmapPixelFormat::Bgra8,
        image.width as i32,
        image.height as i32,
        BitmapAlphaMode::Ignore,
    )
}

impl OcrEngine for WindowsOcr {
    fn name(&self) -> &str {
        "Windows.Media.Ocr"
    }

    fn language(&self) -> Option<String> {
        self.language.clone()
    }

    fn max_image_side(&self) -> u32 {
        self.max_side
    }

    fn recognize(&self, image: &OcrImage<'_>) -> Result<OcrPage, OcrError> {
        let image = OcrImage::new(image.width, image.height, image.rgba)?;
        let side = image.width.max(image.height);
        if side > self.max_side {
            return Err(OcrError::ImageTooLarge {
                side,
                max: self.max_side,
            });
        }
        let bitmap = to_bitmap(&image).map_err(failed)?;
        // `join` blocks this thread until the engine's own worker has finished.
        let result = self
            .engine
            .RecognizeAsync(&bitmap)
            .and_then(|operation| operation.join())
            .map_err(failed)?;
        let mut page = OcrPage {
            lines: Vec::new(),
            text_angle: result.TextAngle().ok().and_then(|angle| angle.Value().ok()),
        };
        for line in result.Lines().map_err(failed)? {
            let mut words = Vec::new();
            for word in line.Words().map_err(failed)? {
                let rect = word.BoundingRect().map_err(failed)?;
                words.push(OcrWord {
                    text: word.Text().map_err(failed)?.to_string(),
                    bbox: [
                        rect.X as f64,
                        rect.Y as f64,
                        (rect.X + rect.Width) as f64,
                        (rect.Y + rect.Height) as f64,
                    ],
                });
            }
            page.lines.push(OcrLine { words });
        }
        Ok(page)
    }
}
