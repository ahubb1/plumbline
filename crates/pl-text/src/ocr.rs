//! The engine-neutral side of text recognition: the trait and its plain data.

/// A rectangle in image pixels: `[x0, y0, x1, y1]`, origin top-left, y down.
pub type PixelRect = [f64; 4];

/// An RGBA8 image to recognise: row-major, top row first, `width * height * 4` bytes.
#[derive(Clone, Copy, Debug)]
pub struct OcrImage<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
}

impl<'a> OcrImage<'a> {
    /// Wraps pixel data, checking that its length fits the size.
    pub fn new(width: u32, height: u32, rgba: &'a [u8]) -> Result<OcrImage<'a>, OcrError> {
        let expected = width as usize * height as usize * 4;
        if width == 0 || height == 0 || rgba.len() != expected {
            return Err(OcrError::BadImage(format!(
                "{width} x {height} px needs {expected} bytes of RGBA, got {}",
                rgba.len()
            )));
        }
        Ok(OcrImage {
            width,
            height,
            rgba,
        })
    }
}

/// One recognised word.
#[derive(Clone, Debug, PartialEq)]
pub struct OcrWord {
    pub text: String,
    pub bbox: PixelRect,
}

/// One recognised line of text, left to right.
#[derive(Clone, Debug, PartialEq)]
pub struct OcrLine {
    pub words: Vec<OcrWord>,
}

impl OcrLine {
    /// The line's words joined by single spaces.
    pub fn text(&self) -> String {
        self.words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// What an engine found on one image.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct OcrPage {
    pub lines: Vec<OcrLine>,
    /// The rotation the engine detected, clockwise in degrees, if it reports one.
    pub text_angle: Option<f64>,
}

impl OcrPage {
    /// Every word, in reading order.
    pub fn words(&self) -> impl Iterator<Item = &OcrWord> {
        self.lines.iter().flat_map(|line| line.words.iter())
    }

    /// The lines joined by newlines.
    pub fn text(&self) -> String {
        self.lines
            .iter()
            .map(OcrLine::text)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Why recognition did not happen. None of these is a reason to fail the document: the
/// feature is simply not available, and the message says why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OcrError {
    /// The engine cannot be used in this process at all (for example the operating system
    /// refuses it to an app without package identity).
    Unavailable(String),
    /// The engine works but has no language to recognise with (no OCR language pack is
    /// installed for any of the user's languages).
    NoLanguage,
    /// The image is not usable as given.
    BadImage(String),
    /// The image is larger than the engine accepts; render it smaller or in strips.
    ImageTooLarge { side: u32, max: u32 },
    /// The engine started and then failed.
    Failed(String),
}

impl std::fmt::Display for OcrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OcrError::Unavailable(why) => write!(f, "text recognition is not available: {why}"),
            OcrError::NoLanguage => write!(
                f,
                "text recognition has no language to work with: no OCR language pack is installed"
            ),
            OcrError::BadImage(why) => write!(f, "the image cannot be recognised: {why}"),
            OcrError::ImageTooLarge { side, max } => write!(
                f,
                "the image is {side} px on its longer side; the engine accepts at most {max} px"
            ),
            OcrError::Failed(why) => write!(f, "text recognition failed: {why}"),
        }
    }
}

impl std::error::Error for OcrError {}

/// A text recogniser. Implementations are created by a fallible constructor of their own, so
/// "this engine cannot be used here" is known before any image is prepared.
pub trait OcrEngine {
    /// A short name for messages and logs, for example `Windows.Media.Ocr`.
    fn name(&self) -> &str;

    /// The language the engine recognises with, as a BCP-47 tag, when it has one fixed.
    fn language(&self) -> Option<String>;

    /// The longest image side the engine accepts, in pixels.
    fn max_image_side(&self) -> u32;

    /// Recognises the text on one image. Blocks until the engine is done; call it from a
    /// worker thread.
    fn recognize(&self, image: &OcrImage<'_>) -> Result<OcrPage, OcrError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in engine: the trait is usable without any real recogniser behind it.
    struct Canned(OcrPage);

    impl OcrEngine for Canned {
        fn name(&self) -> &str {
            "canned"
        }
        fn language(&self) -> Option<String> {
            None
        }
        fn max_image_side(&self) -> u32 {
            64
        }
        fn recognize(&self, image: &OcrImage<'_>) -> Result<OcrPage, OcrError> {
            let side = image.width.max(image.height);
            if side > self.max_image_side() {
                return Err(OcrError::ImageTooLarge {
                    side,
                    max: self.max_image_side(),
                });
            }
            Ok(self.0.clone())
        }
    }

    fn word(text: &str, x: f64) -> OcrWord {
        OcrWord {
            text: text.to_owned(),
            bbox: [x, 0.0, x + 10.0, 8.0],
        }
    }

    #[test]
    fn images_are_checked_against_their_size() {
        let pixels = vec![0u8; 2 * 3 * 4];
        assert!(OcrImage::new(2, 3, &pixels).is_ok());
        assert!(matches!(
            OcrImage::new(3, 3, &pixels),
            Err(OcrError::BadImage(_))
        ));
        assert!(matches!(
            OcrImage::new(0, 3, &[]),
            Err(OcrError::BadImage(_))
        ));
    }

    #[test]
    fn a_page_reads_as_lines_of_words_through_the_trait() {
        let page = OcrPage {
            lines: vec![
                OcrLine {
                    words: vec![word("Door", 0.0), word("D-12", 14.0)],
                },
                OcrLine {
                    words: vec![word("Level", 0.0)],
                },
            ],
            text_angle: Some(0.0),
        };
        let engine: Box<dyn OcrEngine> = Box::new(Canned(page.clone()));
        let small = vec![255u8; 8 * 8 * 4];
        let found = engine
            .recognize(&OcrImage::new(8, 8, &small).unwrap())
            .unwrap();
        assert_eq!(found, page);
        assert_eq!(found.text(), "Door D-12\nLevel");
        assert_eq!(found.words().count(), 3);
        assert_eq!(found.lines[0].text(), "Door D-12");

        let big = vec![255u8; 100 * 2 * 4];
        let err = engine
            .recognize(&OcrImage::new(100, 2, &big).unwrap())
            .unwrap_err();
        assert_eq!(err, OcrError::ImageTooLarge { side: 100, max: 64 });
        assert!(err.to_string().contains("100 px"));
        assert!(OcrError::NoLanguage.to_string().contains("language pack"));
    }
}
