//! Shared helpers for the integration tests: turning an emitted [`Appearance`] into a real PDF
//! (with pdf-writer; the app does this with lopdf in pl-pdf), rendering it with PDFium, and
//! comparing bitmaps.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use pdf_writer::types::AnnotationType;
use pdf_writer::{Content, Finish, Name, Pdf, Ref};
use pl_draw::kurbo::{Affine, Rect};
use pl_draw::{Appearance, BlendMode, DrawList, TransparencyGroup, XObject};
use pl_geom::{PageGeometry, Rotation};
use pl_render::{Bitmap, DocSource, PdfiumBackend, RenderBackend, RenderOptions};

/// The vendored PDFium, as an absolute path.
pub fn vendor_dll() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("vendor")
        .join("pdfium")
        .join("8125")
        .join("bin")
        .join("pdfium.dll");
    std::path::absolute(p).unwrap()
}

pub fn backend() -> PdfiumBackend {
    PdfiumBackend::new(Some(&vendor_dll())).expect("load vendored pdfium.dll")
}

/// A page layout to test on. Every variant shows the same 240 x 160 unit window, so the test
/// drawings (kept inside x 30..230, y 20..150) are visible on all of them.
#[derive(Clone, Copy, Debug)]
pub struct Variant {
    pub name: &'static str,
    pub media: [f64; 4],
    pub crop: Option<[f64; 4]>,
    pub rotate: i32,
    pub user_unit: f64,
}

pub const PLAIN: Variant = Variant {
    name: "plain",
    media: [0.0, 0.0, 240.0, 160.0],
    crop: None,
    rotate: 0,
    user_unit: 1.0,
};

pub const VARIANTS: [Variant; 6] = [
    PLAIN,
    Variant {
        name: "rotate90",
        rotate: 90,
        ..PLAIN
    },
    Variant {
        name: "rotate180",
        rotate: 180,
        ..PLAIN
    },
    Variant {
        name: "rotate270",
        rotate: 270,
        ..PLAIN
    },
    Variant {
        name: "userunit",
        user_unit: 2.5,
        ..PLAIN
    },
    Variant {
        name: "cropbox",
        media: [0.0, 0.0, 300.0, 220.0],
        crop: Some([20.0, 10.0, 260.0, 170.0]),
        rotate: 0,
        user_unit: 1.0,
    },
];

/// A crop box that is offset and a rotation at once: the hardest view transform.
pub const CROP_ROTATED: Variant = Variant {
    name: "cropbox-rotate90",
    media: [0.0, 0.0, 300.0, 220.0],
    crop: Some([20.0, 10.0, 260.0, 170.0]),
    rotate: 90,
    user_unit: 1.0,
};

impl Variant {
    pub fn geometry(&self) -> PageGeometry {
        let r = |b: [f64; 4]| Rect::new(b[0], b[1], b[2], b[3]);
        PageGeometry::new(
            r(self.media),
            self.crop.map(r),
            Rotation::from_degrees(self.rotate).unwrap(),
            self.user_unit,
        )
    }
}

fn pdf_rect(r: Rect) -> pdf_writer::Rect {
    pdf_writer::Rect::new(r.x0 as f32, r.y0 as f32, r.x1 as f32, r.y1 as f32)
}

struct Alloc(i32);

impl Alloc {
    fn next(&mut self) -> Ref {
        self.0 += 1;
        Ref::new(self.0)
    }
}

/// Writes an appearance as a form XObject, with everything it refers to, and returns its id.
/// This is the reference reading of the plain-data description in `pl_draw::emit`.
fn write_form(
    pdf: &mut Pdf,
    alloc: &mut Alloc,
    form: &Appearance,
    group: Option<TransparencyGroup>,
) -> Ref {
    let mut xobjects: Vec<(String, Ref)> = Vec::new();
    for x in &form.resources.xobjects {
        let id = match &x.object {
            XObject::Form(f) => write_form(pdf, alloc, &f.form, f.group),
            XObject::Image(image) => {
                let mask = image.alpha.as_ref().map(|alpha| {
                    let id = alloc.next();
                    let mut m = pdf.image_xobject(id, alpha);
                    m.width(image.width as i32);
                    m.height(image.height as i32);
                    m.color_space().device_gray();
                    m.bits_per_component(8);
                    m.finish();
                    id
                });
                let id = alloc.next();
                let mut i = pdf.image_xobject(id, &image.rgb);
                i.width(image.width as i32);
                i.height(image.height as i32);
                i.color_space().device_rgb();
                i.bits_per_component(8);
                if let Some(mask) = mask {
                    i.s_mask(mask);
                }
                i.finish();
                id
            }
        };
        xobjects.push((x.name.clone(), id));
    }
    let mut fonts: Vec<(String, Ref)> = Vec::new();
    for f in &form.resources.fonts {
        let id = alloc.next();
        pdf.type1_font(id)
            .base_font(Name(f.base_font.as_bytes()))
            .encoding_predefined(Name(f.encoding.as_bytes()));
        fonts.push((f.name.clone(), id));
    }

    let id = alloc.next();
    let mut x = pdf.form_xobject(id, &form.content);
    x.bbox(pdf_rect(form.bbox));
    let m = form.matrix.as_coeffs();
    x.matrix([
        m[0] as f32,
        m[1] as f32,
        m[2] as f32,
        m[3] as f32,
        m[4] as f32,
        m[5] as f32,
    ]);
    if let Some(g) = group {
        x.group()
            .transparency()
            .isolated(g.isolated)
            .knockout(g.knockout);
    }
    let mut res = x.resources();
    if !form.resources.ext_gstates.is_empty() {
        let mut dict = res.ext_g_states();
        for g in &form.resources.ext_gstates {
            let mut gs = dict
                .insert(Name(g.name.as_bytes()))
                .start::<pdf_writer::writers::ExtGraphicsState>();
            gs.stroking_alpha(g.stroke_alpha as f32);
            gs.non_stroking_alpha(g.fill_alpha as f32);
            gs.blend_mode(match g.blend {
                BlendMode::Normal => pdf_writer::types::BlendMode::Normal,
                BlendMode::Multiply => pdf_writer::types::BlendMode::Multiply,
            });
        }
    }
    if !fonts.is_empty() {
        let mut dict = res.fonts();
        for (name, id) in &fonts {
            dict.pair(Name(name.as_bytes()), *id);
        }
    }
    if !xobjects.is_empty() {
        let mut dict = res.x_objects();
        for (name, id) in &xobjects {
            dict.pair(Name(name.as_bytes()), *id);
        }
    }
    res.finish();
    x.finish();
    id
}

/// The page content every test document has under its markup: white paper, a pale blue panel,
/// a mid-grey panel, black bars like lines of text, and one dark red bar. A Multiply markup must
/// leave the bars dark; a Normal one covers them. The picture is laid out for the 240 x 160
/// window and scaled to the visible box, so it also fills a page of any other size.
pub fn backdrop_content(variant: &Variant) -> Vec<u8> {
    let mut c = Content::new();
    let m = variant.media;
    let v = variant.crop.unwrap_or(variant.media);
    let standard = variant.media == PLAIN.media || variant.crop == CROP_ROTATED.crop;
    if !standard {
        c.transform([
            ((v[2] - v[0]) / 240.0) as f32,
            0.0,
            0.0,
            ((v[3] - v[1]) / 160.0) as f32,
            v[0] as f32,
            v[1] as f32,
        ]);
    }
    c.set_fill_rgb(1.0, 1.0, 1.0);
    if standard {
        c.rect(
            m[0] as f32,
            m[1] as f32,
            (m[2] - m[0]) as f32,
            (m[3] - m[1]) as f32,
        );
    } else {
        c.rect(0.0, 0.0, 240.0, 160.0);
    }
    c.fill_nonzero();
    c.set_fill_rgb(0.8, 0.9, 1.0);
    c.rect(130.0, 20.0, 100.0, 130.0);
    c.fill_nonzero();
    c.set_fill_rgb(0.55, 0.55, 0.55);
    c.rect(30.0, 20.0, 40.0, 60.0);
    c.fill_nonzero();
    c.set_fill_rgb(0.0, 0.0, 0.0);
    for i in 0..6 {
        c.rect(40.0, 30.0 + 22.0 * i as f32, 180.0, 4.0);
        c.fill_nonzero();
    }
    c.set_fill_rgb(0.6, 0.0, 0.0);
    c.rect(118.0, 20.0, 5.0, 130.0);
    c.fill_nonzero();
    c.finish().into_vec()
}

/// How the appearance is attached to the test page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attach {
    /// As the `/AP /N` of an annotation of this subtype, `/Rect` equal to the `/BBox`.
    Annotation(AnnotationType),
    /// No markup at all: the backdrop only.
    Nothing,
}

/// Builds a one-page PDF: the backdrop as page content and `appearance` attached as asked.
pub fn build_pdf(variant: &Variant, appearance: &Appearance, attach: Attach) -> Vec<u8> {
    let mut alloc = Alloc(0);
    let catalog = alloc.next();
    let pages = alloc.next();
    let page = alloc.next();
    let content = alloc.next();
    let mut pdf = Pdf::new();
    pdf.catalog(catalog).pages(pages);
    pdf.pages(pages).kids([page]).count(1);
    pdf.stream(content, &backdrop_content(variant));

    let annot = match attach {
        Attach::Annotation(subtype) => {
            let form = write_form(&mut pdf, &mut alloc, appearance, None);
            let id = alloc.next();
            let mut a = pdf.annotation(id);
            a.subtype(subtype);
            a.rect(pdf_rect(appearance.bbox));
            a.flags(pdf_writer::types::AnnotationFlags::PRINT);
            a.appearance().normal().stream(form);
            a.finish();
            Some(id)
        }
        Attach::Nothing => None,
    };

    let mut p = pdf.page(page);
    p.parent(pages);
    let r = |b: [f64; 4]| pdf_writer::Rect::new(b[0] as f32, b[1] as f32, b[2] as f32, b[3] as f32);
    p.media_box(r(variant.media));
    if let Some(c) = variant.crop {
        p.crop_box(r(c));
    }
    if variant.rotate != 0 {
        p.rotate(variant.rotate);
    }
    if variant.user_unit != 1.0 {
        p.user_unit(variant.user_unit as f32);
    }
    p.contents(content);
    p.resources();
    if let Some(a) = annot {
        p.annotations([a]);
    }
    p.finish();
    pdf.finish()
}

/// Pixels per user-space unit for the comparison renders.
pub const SCALE: f64 = 3.0;

/// What one comparison produced.
pub struct Rendered {
    /// PDFium's render of the page without the markup.
    pub backdrop: Bitmap,
    /// PDFium's render with the markup drawn from the emitted appearance stream.
    pub pdfium: Bitmap,
    /// The tiny-skia raster of the same list over `backdrop`.
    pub ours: Bitmap,
}

/// Emits `list`, attaches it to a page of `variant`, renders it with PDFium, and rasterises the
/// same list with tiny-skia over PDFium's render of the bare page.
pub fn render_both(
    backend: &PdfiumBackend,
    list: &DrawList,
    variant: &Variant,
    subtype: AnnotationType,
) -> Rendered {
    render_both_at(backend, list, variant, subtype, SCALE)
}

/// [`render_both`] at a chosen number of pixels per user-space unit.
pub fn render_both_at(
    backend: &PdfiumBackend,
    list: &DrawList,
    variant: &Variant,
    subtype: AnnotationType,
    scale: f64,
) -> Rendered {
    let appearance = pl_draw::emit(list).expect("emit");
    let bytes = build_pdf(variant, &appearance, Attach::Annotation(subtype));
    let info = backend
        .open(DocSource::Bytes(bytes), None)
        .expect("PDFium opens the test document");
    let doc = info.doc;
    let backdrop = backend
        .render_page_with(doc, 0, scale, RenderOptions { annotations: false })
        .unwrap();
    let pdfium = backend
        .render_page_with(doc, 0, scale, RenderOptions { annotations: true })
        .unwrap();
    backend.close(doc).unwrap();

    let transform: Affine = pl_draw::page_transform(&variant.geometry(), scale);
    let pixels = pl_draw::render_over_rgba(
        list,
        transform,
        &backdrop.pixels,
        backdrop.width,
        backdrop.height,
    )
    .expect("rasterise");
    let ours = Bitmap {
        width: backdrop.width,
        height: backdrop.height,
        pixels,
    };
    Rendered {
        backdrop,
        pdfium,
        ours,
    }
}

/// Result of comparing two bitmaps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Diff {
    /// Pixels of either image whose value is not within `tolerance` of the range of values the
    /// other image has in the surrounding `(2 * radius + 1)^2` pixels. Anti-aliasing and
    /// sub-pixel placement differences leave this at zero; a missing, shifted or miscoloured
    /// shape does not.
    pub bad: usize,
    /// Mean absolute difference per channel over the whole image, in grey levels.
    pub mean: f64,
    /// Number of pixels that differ at all between the two images.
    pub touched: usize,
}

fn one_way(a: &Bitmap, b: &Bitmap, radius: i32, tolerance: i32) -> usize {
    bad_pixels(a, b, radius, tolerance).len()
}

/// The pixels of `a` that are out of tolerance against `b` (one direction of [`diff`]).
pub fn bad_pixels(a: &Bitmap, b: &Bitmap, radius: i32, tolerance: i32) -> Vec<(i32, i32)> {
    let (w, h) = (a.width as i32, a.height as i32);
    let mut bad = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let ia = ((y * w + x) * 4) as usize;
            if a.pixels[ia..ia + 3] == b.pixels[ia..ia + 3] {
                continue;
            }
            let mut lo = [255i32; 3];
            let mut hi = [0i32; 3];
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx < 0 || ny < 0 || nx >= w || ny >= h {
                        continue;
                    }
                    let ib = ((ny * w + nx) * 4) as usize;
                    for c in 0..3 {
                        let v = b.pixels[ib + c] as i32;
                        lo[c] = lo[c].min(v);
                        hi[c] = hi[c].max(v);
                    }
                }
            }
            let off = (0..3).any(|c| {
                let v = a.pixels[ia + c] as i32;
                v < lo[c] - tolerance || v > hi[c] + tolerance
            });
            if off {
                bad.push((x, y));
            }
        }
    }
    bad
}

pub fn diff(a: &Bitmap, b: &Bitmap, radius: i32, tolerance: i32) -> Diff {
    assert_eq!((a.width, a.height), (b.width, b.height), "bitmap sizes");
    let mut sum = 0u64;
    let mut touched = 0;
    for (pa, pb) in a
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.pixels.as_chunks::<4>().0)
    {
        let d: u64 = (0..3)
            .map(|c| (pa[c] as i64 - pb[c] as i64).unsigned_abs())
            .sum();
        sum += d;
        if d != 0 {
            touched += 1;
        }
    }
    Diff {
        bad: one_way(a, b, radius, tolerance) + one_way(b, a, radius, tolerance),
        mean: sum as f64 / (a.pixels.len() as f64 * 0.75),
        touched,
    }
}

/// Number of pixels that differ by more than `threshold` in any channel.
pub fn changed_pixels(a: &Bitmap, b: &Bitmap, threshold: i32) -> usize {
    a.pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.pixels.as_chunks::<4>().0)
        .filter(|(pa, pb)| (0..3).any(|c| (pa[c] as i32 - pb[c] as i32).abs() > threshold))
        .count()
}

/// Writes a bitmap as a PNG under the test temp directory, for looking at a failure.
pub fn dump(bitmap: &Bitmap, name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("pl-draw");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.png"));
    let pixmap = pl_draw::pixmap_from_rgba(&bitmap.pixels, bitmap.width, bitmap.height).unwrap();
    pixmap.save_png(&path).unwrap();
    path
}
