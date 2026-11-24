//! Pictures for image stamps: importing a PNG or JPEG file into the RGBA form pl-annot draws,
//! and carrying a picture through the journal, the clipboard and the command file.
//!
//! # Import
//!
//! [`import_image`] decodes PNG or JPEG bytes (nothing else is compiled in), applies the EXIF
//! orientation a camera recorded, and scales a picture with more than [`MAX_IMAGE_PIXELS`]
//! pixels down to that, so one pasted photograph cannot add hundreds of megabytes to a
//! drawing. Dimensions beyond [`MAX_SOURCE_SIDE`] are refused before any pixel is decoded.
//!
//! # Serialised form
//!
//! [`ImageData`] is a shared picture as plain data: its content key, its size and the pixels
//! as a base64 PNG. Inside one journal a picture is written in full once; later records name
//! it by key only (a drag of an image stamp would otherwise write the whole picture with
//! every mouse move). Reading resolves a key through a process-wide table of the pictures
//! that are alive, so records that share a picture share its memory again after a replay.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
use pl_annot::RgbaImage;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The most pixels an imported picture keeps (4 megapixels: 16 MiB as RGBA).
pub const MAX_IMAGE_PIXELS: u64 = 4_000_000;
/// The longest side, in pixels, of a file that is decoded at all.
pub const MAX_SOURCE_SIDE: u32 = 20_000;
/// The resolution a picture is taken to have when it is first placed: 96 pixels per inch.
pub const PLACEMENT_DPI: f64 = 96.0;

/// Why a picture could not be imported.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("read {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the picture is neither a PNG nor a JPEG file")]
    UnknownFormat,
    #[error("{format} pictures cannot be imported; use PNG or JPEG")]
    Unsupported { format: String },
    #[error("the picture is {width} x {height} pixels; the largest side that can be imported is {MAX_SOURCE_SIDE}")]
    TooLarge { width: u32, height: u32 },
    #[error("the picture could not be decoded: {0}")]
    Decode(String),
}

/// A picture ready to be placed as an image stamp.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedImage {
    /// Straight-alpha RGBA, shared.
    pub image: Arc<RgbaImage>,
    /// The size of the picture in the file, after its EXIF orientation was applied.
    pub source_size: (u32, u32),
    /// Whether it was scaled down to [`MAX_IMAGE_PIXELS`].
    pub downscaled: bool,
    /// `PNG` or `JPEG`.
    pub format: &'static str,
}

impl ImportedImage {
    /// The size, in page units, the picture has at [`PLACEMENT_DPI`] (from the size in the
    /// file, so a picture that was scaled down is placed as large as it would have been).
    pub fn natural_size(&self) -> (f64, f64) {
        let k = 72.0 / PLACEMENT_DPI;
        (
            f64::from(self.source_size.0) * k,
            f64::from(self.source_size.1) * k,
        )
    }

    /// The natural size shrunk (never grown) to fit inside `max_width` x `max_height`, keeping
    /// the picture's proportions: the size to place it at on a page.
    pub fn size_within(&self, max_width: f64, max_height: f64) -> (f64, f64) {
        let (w, h) = self.natural_size();
        let k = (max_width / w).min(max_height / h).min(1.0);
        if k.is_finite() && k > 0.0 {
            (w * k, h * k)
        } else {
            (w, h)
        }
    }
}

/// The size a `width` x `height` picture is scaled to so that it has at most `max_pixels`
/// pixels, keeping its proportions; unchanged when it is small enough already.
pub fn fitted_size(width: u32, height: u32, max_pixels: u64) -> (u32, u32) {
    let pixels = u64::from(width) * u64::from(height);
    if pixels <= max_pixels || pixels == 0 {
        return (width, height);
    }
    let k = (max_pixels as f64 / pixels as f64).sqrt();
    let mut w = ((f64::from(width) * k).floor() as u32).max(1);
    let mut h = ((f64::from(height) * k).floor() as u32).max(1);
    // Rounding must never push the product back over the limit.
    while u64::from(w) * u64::from(h) > max_pixels {
        if w >= h {
            w -= 1;
        } else {
            h -= 1;
        }
    }
    (w, h)
}

/// Decodes PNG or JPEG bytes into a picture for [`pl_annot::Annotation::image_stamp`].
pub fn import_image(bytes: &[u8]) -> Result<ImportedImage, ImportError> {
    import_image_capped(bytes, MAX_IMAGE_PIXELS)
}

/// As [`import_image`] with a chosen pixel limit.
pub fn import_image_capped(bytes: &[u8], max_pixels: u64) -> Result<ImportedImage, ImportError> {
    let decode = |e: image::ImageError| ImportError::Decode(e.to_string());
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| ImportError::Decode(e.to_string()))?;
    let format = match reader.format() {
        Some(ImageFormat::Png) => "PNG",
        Some(ImageFormat::Jpeg) => "JPEG",
        Some(other) => {
            return Err(ImportError::Unsupported {
                format: format!("{other:?}"),
            })
        }
        None => return Err(ImportError::UnknownFormat),
    };
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_SIDE);
    limits.max_image_height = Some(MAX_SOURCE_SIDE);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|e| match e {
        image::ImageError::Limits(_) => {
            // The header was readable; say how large the picture is.
            let (width, height) = ImageReader::new(Cursor::new(bytes))
                .with_guessed_format()
                .ok()
                .and_then(|r| r.into_dimensions().ok())
                .unwrap_or((0, 0));
            ImportError::TooLarge { width, height }
        }
        other => decode(other),
    })?;
    let (w, h) = decoder.dimensions();
    if w > MAX_SOURCE_SIDE || h > MAX_SOURCE_SIDE {
        return Err(ImportError::TooLarge {
            width: w,
            height: h,
        });
    }
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut picture = DynamicImage::from_decoder(decoder).map_err(decode)?;
    picture.apply_orientation(orientation);
    let source_size = (picture.width(), picture.height());
    let (tw, th) = fitted_size(source_size.0, source_size.1, max_pixels.max(1));
    let downscaled = (tw, th) != source_size;
    let rgba = if downscaled {
        // RGBA first: the resize then filters colour and alpha alike.
        image::imageops::resize(
            &picture.into_rgba8(),
            tw,
            th,
            image::imageops::FilterType::Triangle,
        )
    } else {
        picture.into_rgba8()
    };
    let (width, height) = rgba.dimensions();
    let image = RgbaImage::new(width, height, rgba.into_raw())
        .map_err(|e| ImportError::Decode(e.to_string()))?;
    Ok(ImportedImage {
        image: Arc::new(image),
        source_size,
        downscaled,
        format,
    })
}

/// Reads and imports a picture file (see [`import_image`]).
pub fn import_image_file(path: &Path) -> Result<ImportedImage, ImportError> {
    let bytes = std::fs::read(path).map_err(|source| ImportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    import_image(&bytes)
}

// ---- PNG and base64 --------------------------------------------------------------------------

/// The picture as a PNG file (lossless, alpha kept).
pub fn encode_png(image: &RgbaImage) -> Vec<u8> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(
            image.pixels(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .expect("writing a PNG into memory cannot fail");
    out
}

fn decode_png(bytes: &[u8]) -> Result<RgbaImage, String> {
    let picture = image::load_from_memory_with_format(bytes, ImageFormat::Png)
        .map_err(|e| e.to_string())?
        .into_rgba8();
    let (width, height) = picture.dimensions();
    RgbaImage::new(width, height, picture.into_raw()).map_err(|e| e.to_string())
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Decodes standard base64 (padding optional, white space ignored). `None` for anything else.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    let mut ended = false;
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                ended = true;
                continue;
            }
            b' ' | b'\r' | b'\n' | b'\t' => continue,
            _ => return None,
        };
        if ended {
            return None;
        }
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // What is left over is padding bits, which must be zero.
    (acc == 0 && bits < 6).then_some(out)
}

// ---- shared pictures as data -----------------------------------------------------------------

fn content_key(image: &RgbaImage) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    eat(&image.width().to_le_bytes());
    eat(&image.height().to_le_bytes());
    eat(image.pixels());
    h
}

#[derive(Default)]
struct Registry {
    /// The pictures that are alive, by content key.
    by_key: HashMap<u64, Weak<RgbaImage>>,
    /// The key of a picture by the address of its allocation, so that it is hashed once.
    by_ptr: HashMap<usize, (Weak<RgbaImage>, u64)>,
}

fn registry() -> std::sync::MutexGuard<'static, Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// The content key of a shared picture, remembered for as long as the picture lives.
fn key_of(image: &Arc<RgbaImage>) -> u64 {
    let ptr = Arc::as_ptr(image) as usize;
    if let Some((weak, key)) = registry().by_ptr.get(&ptr) {
        if weak.upgrade().is_some_and(|live| Arc::ptr_eq(&live, image)) {
            return *key;
        }
    }
    // Hashed outside the lock: a large picture takes a few milliseconds.
    let key = content_key(image);
    let mut reg = registry();
    if reg.by_ptr.len() > 64 {
        reg.by_ptr.retain(|_, (weak, _)| weak.strong_count() > 0);
        reg.by_key.retain(|_, weak| weak.strong_count() > 0);
    }
    reg.by_ptr.insert(ptr, (Arc::downgrade(image), key));
    reg.by_key
        .entry(key)
        .and_modify(|weak| {
            if weak.strong_count() == 0 {
                *weak = Arc::downgrade(image);
            }
        })
        .or_insert_with(|| Arc::downgrade(image));
    key
}

fn live_image(key: u64, width: u32, height: u32) -> Option<Arc<RgbaImage>> {
    registry()
        .by_key
        .get(&key)
        .and_then(Weak::upgrade)
        .filter(|i| i.width() == width && i.height() == height)
}

thread_local! {
    /// The pictures the journal being written already holds; `None` outside a journal write.
    static KNOWN: RefCell<Option<HashSet<u64>>> = const { RefCell::new(None) };
}

/// Runs `f` (a serialisation) so that a picture whose key is in `known` is written by key
/// only, and every picture that is written in full is added to `known`.
pub(crate) fn eliding_known_images<R>(known: &mut HashSet<u64>, f: impl FnOnce() -> R) -> R {
    struct Restore<'a>(&'a mut HashSet<u64>);
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            if let Some(set) = KNOWN.with(|k| k.borrow_mut().take()) {
                *self.0 = set;
            }
        }
    }
    KNOWN.with(|k| *k.borrow_mut() = Some(std::mem::take(known)));
    let _restore = Restore(known);
    f()
}

/// A shared picture as plain serialisable data (see the module documentation).
#[derive(Clone, Debug, PartialEq)]
pub struct ImageData(pub Arc<RgbaImage>);

#[derive(Serialize, Deserialize)]
struct ImageRecord {
    /// The content key, sixteen hexadecimal digits.
    key: String,
    width: u32,
    height: u32,
    /// The picture as a base64 PNG; left out when the journal already holds this key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    png: Option<String>,
}

impl Serialize for ImageData {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let key = key_of(&self.0);
        let elide = KNOWN.with(|k| match k.borrow_mut().as_mut() {
            Some(known) => !known.insert(key),
            None => false,
        });
        ImageRecord {
            key: format!("{key:016x}"),
            width: self.0.width(),
            height: self.0.height(),
            png: (!elide).then(|| base64_encode(&encode_png(&self.0))),
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for ImageData {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<ImageData, D::Error> {
        use serde::de::Error;
        let record = ImageRecord::deserialize(d)?;
        let key = u64::from_str_radix(&record.key, 16)
            .map_err(|_| D::Error::custom("an image key is not hexadecimal"))?;
        let Some(png) = record.png else {
            return live_image(key, record.width, record.height)
                .map(ImageData)
                .ok_or_else(|| {
                    D::Error::custom(format!(
                        "image {} is named but not stored in this data",
                        record.key
                    ))
                });
        };
        let bytes =
            base64_decode(&png).ok_or_else(|| D::Error::custom("an image is not valid base64"))?;
        let image = decode_png(&bytes).map_err(D::Error::custom)?;
        if (image.width(), image.height()) != (record.width, record.height) {
            return Err(D::Error::custom(
                "an image does not have the size it states",
            ));
        }
        // The key is taken from the pixels, not from the record: a key that lies cannot make
        // one picture stand in for another.
        let actual = content_key(&image);
        if let Some(live) = live_image(actual, image.width(), image.height()) {
            if live.pixels() == image.pixels() {
                return Ok(ImageData(live));
            }
        }
        let image = Arc::new(image);
        {
            let mut reg = registry();
            reg.by_key.insert(actual, Arc::downgrade(&image));
            reg.by_ptr.insert(
                Arc::as_ptr(&image) as usize,
                (Arc::downgrade(&image), actual),
            );
            if actual != key {
                // Later records name it by the key it was written under.
                reg.by_key.insert(key, Arc::downgrade(&image));
            }
        }
        Ok(ImageData(image))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(width: u32, height: u32, seed: u8) -> RgbaImage {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[
                    (x * 255 / width.max(2).saturating_sub(1).max(1)) as u8,
                    (y * 255 / height.max(2).saturating_sub(1).max(1)) as u8,
                    seed,
                    if (x + y) % 5 == 0 { 96 } else { 255 },
                ]);
            }
        }
        RgbaImage::new(width, height, pixels).unwrap()
    }

    fn jpeg_of(width: u32, height: u32) -> Vec<u8> {
        let rgb: Vec<u8> = (0..width * height)
            .flat_map(|i| {
                let (x, y) = (i % width, i / width);
                [(x * 4) as u8, (y * 4) as u8, 128]
            })
            .collect();
        let mut out = Vec::new();
        use image::ImageEncoder;
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92)
            .write_image(&rgb, width, height, image::ExtendedColorType::Rgb8)
            .unwrap();
        out
    }

    #[test]
    fn base64_round_trips_every_length_and_refuses_rubbish() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        for n in 0..70usize {
            let data: Vec<u8> = (0..n).map(|i| (i * 37 + n * 11) as u8).collect();
            let text = base64_encode(&data);
            assert_eq!(base64_decode(&text).as_deref(), Some(&data[..]), "{n}");
            // Padding is optional, white space is ignored.
            let loose = text.trim_end_matches('=').replace("A", "A\n");
            assert_eq!(base64_decode(&loose).as_deref(), Some(&data[..]), "{n}");
        }
        assert_eq!(base64_decode("Zm9v!"), None);
        assert_eq!(base64_decode("Zg==Zg"), None, "data after padding");
        assert_eq!(base64_decode("Zh"), None, "non-zero padding bits");
    }

    #[test]
    fn a_png_with_alpha_is_imported_exactly() {
        let source = gradient(37, 21, 7);
        let png = encode_png(&source);
        let got = import_image(&png).unwrap();
        assert_eq!(got.format, "PNG");
        assert_eq!(got.source_size, (37, 21));
        assert!(!got.downscaled);
        assert_eq!(*got.image, source, "pixels and alpha survive");
        assert!(!got.image.is_opaque());
        let (w, h) = got.natural_size();
        assert!((w - 37.0 * 0.75).abs() < 1e-9 && (h - 21.0 * 0.75).abs() < 1e-9);
        let (fw, fh) = got.size_within(10.0, 100.0);
        assert!((fw - 10.0).abs() < 1e-9 && (fh / fw - 21.0 / 37.0).abs() < 1e-9);
        assert_eq!(got.size_within(1000.0, 1000.0), (w, h), "never enlarged");
    }

    #[test]
    fn a_jpeg_is_imported_as_opaque_rgba() {
        let got = import_image(&jpeg_of(48, 32)).unwrap();
        assert_eq!(got.format, "JPEG");
        assert_eq!((got.image.width(), got.image.height()), (48, 32));
        assert!(got.image.is_opaque());
        // Lossy, but close: the gradient's corners are still what was drawn.
        let px = |x: u32, y: u32| {
            let i = ((y * 48 + x) * 4) as usize;
            [
                got.image.pixels()[i],
                got.image.pixels()[i + 1],
                got.image.pixels()[i + 2],
            ]
        };
        let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) < 24);
        assert!(near(px(0, 0), [0, 0, 128]), "{:?}", px(0, 0));
        assert!(near(px(47, 31), [188, 124, 128]), "{:?}", px(47, 31));
    }

    #[test]
    fn a_huge_picture_is_scaled_down_and_keeps_its_proportions() {
        assert_eq!(fitted_size(100, 50, 10_000), (100, 50));
        let (w, h) = fitted_size(8000, 6000, MAX_IMAGE_PIXELS);
        assert!(u64::from(w) * u64::from(h) <= MAX_IMAGE_PIXELS);
        assert!(
            u64::from(w + 2) * u64::from(h + 2) > MAX_IMAGE_PIXELS,
            "not smaller than needed"
        );
        assert!((f64::from(w) / f64::from(h) - 8000.0 / 6000.0).abs() < 0.01);
        assert_eq!(fitted_size(1_000_000, 1, 1000), (1000, 1));
        assert_eq!(fitted_size(3, 1_000_000, 10), (1, 10));

        // A real decode with a small cap: 300 x 200 into at most 6,000 pixels.
        let png = encode_png(&gradient(300, 200, 1));
        let got = import_image_capped(&png, 6_000).unwrap();
        assert!(got.downscaled);
        assert_eq!(got.source_size, (300, 200));
        let (w, h) = (got.image.width(), got.image.height());
        assert_eq!((w, h), fitted_size(300, 200, 6_000));
        assert!(w * h <= 6_000 && w > 80 && h > 50);
        // Placed as large as the original would have been.
        assert_eq!(got.natural_size(), (225.0, 150.0));
        // The left edge is still dark red-wise and the right edge bright.
        let p = got.image.pixels();
        let mid = (h / 2 * w * 4) as usize;
        assert!(p[mid] < 30 && p[mid + (w as usize - 1) * 4] > 225);
    }

    #[test]
    fn what_is_not_a_png_or_jpeg_is_refused_with_a_reason() {
        assert!(matches!(
            import_image(b"hello, not a picture"),
            Err(ImportError::UnknownFormat)
        ));
        assert!(matches!(
            import_image(b"GIF89a\x01\x00\x01\x00\x00\x00\x00;"),
            Err(ImportError::Unsupported { .. })
        ));
        let mut cut = encode_png(&gradient(40, 40, 3));
        cut.truncate(cut.len() / 2);
        assert!(matches!(import_image(&cut), Err(ImportError::Decode(_))));
        assert!(matches!(
            import_image_file(Path::new("Z:\\no\\such\\picture.png")),
            Err(ImportError::Io { .. })
        ));
        // A header that claims an enormous picture is refused before anything is decoded.
        let mut huge = encode_png(&gradient(4, 4, 3));
        huge[16..20].copy_from_slice(&50_000u32.to_be_bytes());
        // (The header checksum no longer matches; either refusal is right.)
        assert!(matches!(
            import_image(&huge),
            Err(ImportError::TooLarge { .. } | ImportError::Decode(_))
        ));
    }

    #[test]
    fn image_data_round_trips_and_a_journal_writes_the_pixels_once() {
        let image = Arc::new(gradient(64, 48, 9));
        let data = ImageData(image.clone());
        let full = serde_json::to_string(&data).unwrap();
        assert!(full.contains("\"png\""));
        let back: ImageData = serde_json::from_str(&full).unwrap();
        assert_eq!(back, data);
        assert!(
            Arc::ptr_eq(&back.0, &image),
            "a live picture is shared, not copied"
        );

        // Inside one journal: in full the first time, by key afterwards.
        let mut known = HashSet::new();
        let first = eliding_known_images(&mut known, || serde_json::to_string(&data).unwrap());
        assert_eq!(first, full);
        assert_eq!(known.len(), 1);
        let second = eliding_known_images(&mut known, || serde_json::to_string(&data).unwrap());
        assert!(!second.contains("png") && second.len() < 80, "{second}");
        let by_key: ImageData = serde_json::from_str(&second).unwrap();
        assert!(Arc::ptr_eq(&by_key.0, &image));
        // Outside the journal everything is written in full again.
        assert_eq!(serde_json::to_string(&data).unwrap(), full);

        // Once nothing holds the picture, a key alone cannot bring it back; the full form can.
        drop((back, by_key, data));
        let reference = (*image).clone();
        drop(image);
        assert!(serde_json::from_str::<ImageData>(&second).is_err());
        let revived: ImageData = serde_json::from_str(&full).unwrap();
        assert_eq!(*revived.0, reference);
        assert!(serde_json::from_str::<ImageData>(&second).is_ok());

        // Damaged data is an error, not a wrong picture.
        let bad = full.replace("\"width\":64", "\"width\":63");
        assert!(serde_json::from_str::<ImageData>(&bad).is_err());
        let bad = full.replace("\"png\":\"", "\"png\":\"!!");
        assert!(serde_json::from_str::<ImageData>(&bad).is_err());
    }
}
