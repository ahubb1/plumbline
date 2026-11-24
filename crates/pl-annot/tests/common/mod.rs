//! Shared helpers for the integration tests: fixture pages, sample annotations of every kind,
//! saving through pl-pdf, rendering with PDFium and comparing bitmaps.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use pl_annot::{
    AnnotKind, AnnotSet, Annotation, DrawList, LineEnding, LinkTarget, Origin, PdfDate, Rgb,
    SavePlan,
};
use pl_geom::{PageGeometry, Point, Rect, Vec2};
use pl_pdf::{ObjectId, PdfDocument, PdfFile, SaveOptions, SaveReport, SecondParser};
use pl_render::{Bitmap, DocSource, PdfiumBackend, RenderBackend, RenderOptions};
use pl_testkit::Fixture;

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

pub fn origin() -> Origin {
    Origin::new(
        "Plumbline Tester",
        PdfDate::parse("D:20261124101500Z").unwrap(),
    )
}

/// One page of a pl-testkit fixture to run a test on.
#[derive(Clone, Copy)]
pub struct Target {
    pub name: &'static str,
    pub fixture: fn() -> Fixture,
    pub page: usize,
}

/// A plain page, the three rotated pages, the `/UserUnit` page, and the three box-offset pages
/// (inset CropBox; MediaBox with a non-zero origin; both plus `/Rotate 90`).
pub const TARGETS: [Target; 8] = [
    Target {
        name: "plain",
        fixture: pl_testkit::plain,
        page: 0,
    },
    Target {
        name: "rotate90",
        fixture: pl_testkit::rotated_pages,
        page: 1,
    },
    Target {
        name: "rotate180",
        fixture: pl_testkit::rotated_pages,
        page: 2,
    },
    Target {
        name: "rotate270",
        fixture: pl_testkit::rotated_pages,
        page: 3,
    },
    Target {
        name: "userunit",
        fixture: pl_testkit::user_unit,
        page: 0,
    },
    Target {
        name: "cropbox",
        fixture: pl_testkit::crop_box,
        page: 0,
    },
    Target {
        name: "mediabox-offset",
        fixture: pl_testkit::crop_box,
        page: 1,
    },
    Target {
        name: "cropbox-rotate90",
        fixture: pl_testkit::crop_box,
        page: 2,
    },
];

/// A fixture written to a temporary directory and opened with pl-pdf.
pub struct Work {
    pub dir: tempfile::TempDir,
    pub path: PathBuf,
    pub file: PdfFile,
    pub original: Vec<u8>,
}

impl Work {
    pub fn new(fixture: &Fixture) -> Work {
        Work::from_bytes(&fixture.manifest.file_name(), fixture.bytes.clone())
    }

    pub fn from_bytes(name: &str, bytes: Vec<u8>) -> Work {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, &bytes).unwrap();
        let file = PdfFile::open(&path).unwrap();
        Work {
            dir,
            path,
            file,
            original: bytes,
        }
    }

    pub fn doc(&mut self) -> &PdfDocument {
        self.file.document().unwrap()
    }

    pub fn geometry(&mut self, page: usize) -> PageGeometry {
        self.doc().page(page).unwrap().geometry
    }

    /// The visible box's lower-left corner plus a margin: where the samples are placed.
    pub fn sample_origin(&mut self, page: usize) -> Vec2 {
        let crop = self.geometry(page).crop_box();
        Vec2::new(crop.x0 + 24.0, crop.y0 + 24.0)
    }

    /// Plans and performs a verified incremental save of `set`, then checks that the bytes on
    /// disk start with exactly the bytes that were there before.
    pub fn save(&mut self, set: &AnnotSet) -> (SavePlan, SaveReport) {
        let before = std::fs::read(&self.path).unwrap();
        let plan = set.plan_save(self.doc()).expect("plan");
        let report = self
            .file
            .save_incremental(&plan.edits, &SaveOptions::default())
            .expect("verified incremental save");
        let after = std::fs::read(&self.path).unwrap();
        assert!(after.len() > before.len());
        assert!(
            after[..before.len()] == before[..],
            "the previous bytes are an exact prefix"
        );
        assert!(
            after.starts_with(&self.original),
            "the original bytes are an exact prefix"
        );
        let verification = report.verification.as_ref().expect("verification ran");
        assert_eq!(
            verification.second_parser,
            SecondParser::Passed,
            "hayro-syntax re-opened it"
        );
        assert!(verification.prefix_checked);
        (plan, report)
    }

    pub fn bytes(&self) -> Vec<u8> {
        std::fs::read(&self.path).unwrap()
    }
}

/// Every object of `before` except `allowed` must be the same object at the same file offset
/// in `after`: it was not rewritten.
pub fn assert_untouched(before: &PdfDocument, after: &PdfDocument, allowed: &[ObjectId]) {
    let mut checked = 0;
    for id in before.object_ids() {
        if allowed.contains(&id) {
            continue;
        }
        assert_eq!(after.object(id), before.object(id), "object {id:?} changed");
        assert_eq!(
            after.object_offset(id),
            before.object_offset(id),
            "object {id:?} was written again"
        );
        checked += 1;
    }
    assert!(checked > 0);
}

fn pt(o: Vec2, x: f64, y: f64) -> Point {
    Point::new(o.x + x, o.y + y)
}

fn rect(o: Vec2, x0: f64, y0: f64, x1: f64, y1: f64) -> Rect {
    Rect::new(o.x + x0, o.y + y0, o.x + x1, o.y + y1)
}

/// One test case: a name and the annotations it adds (more than one when they belong
/// together, like a note and its popup).
pub struct Sample {
    pub name: &'static str,
    pub annotations: Vec<Annotation>,
}

/// At least one sample of every modelled kind, covering dashes, line endings, opacity, fills,
/// cloud borders and blend modes. All coordinates are multiples of 1/4 inside a 240 x 160
/// window at `o`, so they survive the single-precision numbers of a PDF exactly.
pub fn samples(page: usize, o: Vec2) -> Vec<Sample> {
    let og = origin();
    let blue = Rgb::new(0.0, 0.25, 0.75);
    let green = Rgb::new(0.0, 0.5, 0.25);
    let mut out = Vec::new();
    let mut push = |name: &'static str, annotations: Vec<Annotation>| {
        out.push(Sample { name, annotations });
    };

    let mut a = Annotation::square(page, rect(o, 10.0, 10.0, 130.0, 90.0), &og);
    a.style.width = 3.0;
    a.meta.contents = Some("A plain rectangle".into());
    push("square", vec![a]);

    let mut a = Annotation::square(page, rect(o, 20.5, 30.25, 200.0, 120.0), &og);
    a.style.width = 2.0;
    a.style.stroke = Some(blue);
    a.style.fill = Some(Rgb::new(1.0, 0.75, 0.25));
    a.style.dash = Some(vec![6.0, 3.0]);
    a.style.opacity = 0.5;
    push("dash-square-filled-translucent", vec![a]);

    let mut a = Annotation::square(page, rect(o, 30.0, 30.0, 190.0, 120.0), &og);
    a.style.width = 1.5;
    a.style.stroke = Some(blue);
    a.style.fill = Some(Rgb::new(0.75, 1.0, 0.75));
    a.style.cloud = Some(1.0);
    push("square-cloud", vec![a]);

    let mut a = Annotation::circle(page, rect(o, 10.0, 20.0, 170.0, 110.0), &og);
    a.style.width = 4.0;
    a.style.fill = Some(Rgb::new(0.5, 0.75, 1.0));
    a.style.opacity = 0.75;
    push("circle", vec![a]);

    let mut a = Annotation::circle(page, rect(o, 40.0, 30.0, 200.0, 130.0), &og);
    a.style.width = 2.0;
    a.style.stroke = Some(green);
    a.style.cloud = Some(2.0);
    push("circle-cloud", vec![a]);

    let mut a = Annotation::line(page, pt(o, 20.0, 20.0), pt(o, 200.0, 110.0), &og);
    a.style.width = 2.5;
    a.style.stroke = Some(blue);
    push("line", vec![a]);

    let mut a = Annotation::line(page, pt(o, 30.0, 120.0), pt(o, 210.0, 40.0), &og);
    a.style.width = 3.0;
    a.style.line_start = LineEnding::OpenArrow;
    a.style.line_end = LineEnding::ClosedArrow;
    a.style.fill = Some(Rgb::new(1.0, 1.0, 0.0));
    a.style.opacity = 0.5;
    push("line-arrows-translucent", vec![a]);

    let mut a = Annotation::line(page, pt(o, 20.0, 80.0), pt(o, 220.0, 80.0), &og);
    a.style.width = 2.0;
    a.style.dash = Some(vec![8.0, 4.0, 2.0, 4.0]);
    a.style.line_start = LineEnding::Butt;
    a.style.line_end = LineEnding::Slash;
    push("dash-line-butt-slash", vec![a]);

    let mut a = Annotation::polyline(
        page,
        vec![
            pt(o, 20.0, 30.0),
            pt(o, 80.0, 120.0),
            pt(o, 140.0, 40.0),
            pt(o, 210.0, 110.0),
        ],
        &og,
    );
    a.style.width = 2.0;
    a.style.stroke = Some(green);
    a.style.fill = Some(Rgb::WHITE);
    a.style.line_start = LineEnding::Circle;
    a.style.line_end = LineEnding::Diamond;
    push("polyline-endings", vec![a]);

    let mut a = Annotation::polyline(
        page,
        vec![pt(o, 30.0, 110.0), pt(o, 110.0, 30.0), pt(o, 200.0, 100.0)],
        &og,
    );
    a.style.width = 1.5;
    a.style.line_end = LineEnding::Square;
    push("polyline-square-end", vec![a]);

    let mut a = Annotation::polygon(
        page,
        vec![
            pt(o, 30.0, 20.0),
            pt(o, 200.0, 40.0),
            pt(o, 170.0, 130.0),
            pt(o, 60.0, 110.0),
        ],
        &og,
    );
    a.style.width = 2.0;
    a.style.fill = Some(Rgb::new(0.25, 0.75, 0.5));
    a.style.opacity = 0.5;
    push("polygon-filled-translucent", vec![a]);

    let mut a = Annotation::polygon(
        page,
        vec![
            pt(o, 40.0, 30.0),
            pt(o, 200.0, 30.0),
            pt(o, 200.0, 120.0),
            pt(o, 120.0, 80.0),
            pt(o, 40.0, 120.0),
        ],
        &og,
    );
    a.style.width = 1.5;
    a.style.stroke = Some(blue);
    a.style.cloud = Some(1.0);
    push("polygon-cloud", vec![a]);

    let mut a = Annotation::ink(
        page,
        vec![
            vec![
                pt(o, 20.0, 40.0),
                pt(o, 50.0, 90.0),
                pt(o, 90.0, 50.0),
                pt(o, 130.0, 100.0),
                pt(o, 170.0, 60.0),
            ],
            vec![pt(o, 60.0, 120.0), pt(o, 120.0, 30.0)],
            vec![pt(o, 200.0, 120.0)],
        ],
        &og,
    );
    a.style.width = 3.5;
    a.style.stroke = Some(blue);
    a.style.opacity = 0.75;
    push("ink", vec![a]);

    let quads = vec![
        [
            pt(o, 20.0, 100.0),
            pt(o, 200.0, 100.0),
            pt(o, 200.0, 118.0),
            pt(o, 20.0, 118.0),
        ],
        [
            pt(o, 20.0, 70.0),
            pt(o, 140.0, 70.0),
            pt(o, 140.0, 88.0),
            pt(o, 20.0, 88.0),
        ],
    ];
    let a = Annotation::text_markup(AnnotKind::Highlight, page, quads.clone(), &og);
    push("highlight", vec![a]);

    let slanted = vec![[
        pt(o, 30.0, 30.0),
        pt(o, 190.0, 70.0),
        pt(o, 186.0, 86.0),
        pt(o, 26.0, 46.0),
    ]];
    let mut a = Annotation::text_markup(AnnotKind::Highlight, page, slanted, &og);
    a.style.stroke = Some(Rgb::new(0.5, 1.0, 0.5));
    a.style.opacity = 0.75;
    a.style.blend = pl_annot::BlendMode::Normal;
    push("highlight-slanted-normal", vec![a]);

    for (name, kind, color) in [
        ("underline", AnnotKind::Underline, green),
        ("strikeout", AnnotKind::StrikeOut, Rgb::new(0.75, 0.0, 0.0)),
        ("squiggly", AnnotKind::Squiggly, blue),
    ] {
        let mut a = Annotation::text_markup(kind, page, quads.clone(), &og);
        a.style.stroke = Some(color);
        push(name, vec![a]);
    }

    let mut note = Annotation::note(
        page,
        rect(o, 40.0, 60.0, 76.0, 96.0),
        "A note with a popup",
        &og,
    );
    let mut popup = Annotation::popup_for(&mut note, rect(o, 90.0, 20.0, 230.0, 100.0), &og);
    popup.open = true;
    push("note-popup", vec![note, popup]);

    let link = Annotation::link(
        page,
        rect(o, 20.0, 20.0, 120.0, 50.0),
        LinkTarget::Uri("https://example.org/plan?a=1".into()),
        &og,
    );
    let mut bordered = Annotation::link(
        page,
        rect(o, 20.0, 70.0, 200.0, 130.0),
        LinkTarget::Page {
            page: 0,
            at: Some(Point::new(72.0, 700.0)),
        },
        &og,
    );
    bordered.style.stroke = Some(blue);
    bordered.style.width = 2.0;
    let fit = Annotation::link(
        page,
        rect(o, 130.0, 20.0, 220.0, 50.0),
        LinkTarget::Page { page: 0, at: None },
        &og,
    );
    push("links", vec![link, bordered, fit]);

    out
}

/// One annotation of each modelled kind, for tests that only need "one of each".
pub fn one_of_each(page: usize, o: Vec2) -> Vec<Annotation> {
    let wanted = [
        "square",
        "circle",
        "line",
        "polyline-endings",
        "polygon-filled-translucent",
        "ink",
        "highlight",
        "underline",
        "strikeout",
        "squiggly",
        "note-popup",
        "links",
    ];
    let mut out = Vec::new();
    for sample in samples(page, o) {
        if wanted.contains(&sample.name) {
            let take = if sample.name == "links" {
                1
            } else {
                sample.annotations.len()
            };
            out.extend(sample.annotations.into_iter().take(take));
        }
    }
    out
}

/// Pixels per user-space unit that give a page about 1200 px wide.
pub fn scale_for(geometry: &PageGeometry) -> f64 {
    if geometry.user_unit() > 1.0 {
        4.0
    } else {
        2.0
    }
}

pub struct Rendered {
    /// PDFium's render of the page without annotations.
    pub backdrop: Bitmap,
    /// PDFium's render with annotations drawn from their appearance streams.
    pub pdfium: Bitmap,
}

/// Renders one page of a document twice with PDFium: without and with annotations.
pub fn render_pdf(
    backend: &PdfiumBackend,
    bytes: Vec<u8>,
    page: usize,
    geometry: &PageGeometry,
) -> Rendered {
    let doc = backend
        .open(DocSource::Bytes(bytes), None)
        .expect("PDFium opens the file")
        .doc;
    let page = page as u32;
    backend
        .set_page_user_unit(doc, page, geometry.user_unit())
        .unwrap();
    let scale = scale_for(geometry);
    let backdrop = backend
        .render_page_with(doc, page, scale, RenderOptions { annotations: false })
        .unwrap();
    let pdfium = backend
        .render_page_with(doc, page, scale, RenderOptions { annotations: true })
        .unwrap();
    backend.close(doc).unwrap();
    Rendered { backdrop, pdfium }
}

/// How many annotations PDFium counts on each of the given pages.
pub fn pdfium_annotation_counts(
    backend: &PdfiumBackend,
    source: DocSource,
    pages: &[usize],
) -> Vec<usize> {
    let doc = backend
        .open(source, None)
        .expect("PDFium opens the file")
        .doc;
    let counts = pages
        .iter()
        .map(|p| backend.annotations(doc, *p as u32).unwrap().len())
        .collect();
    backend.close(doc).unwrap();
    counts
}

/// The tiny-skia raster of `list` over a PDFium render of the bare page.
pub fn raster_over(list: &DrawList, geometry: &PageGeometry, backdrop: &Bitmap) -> Bitmap {
    let transform = pl_draw::page_transform(geometry, scale_for(geometry));
    let pixels = pl_draw::render_over_rgba(
        list,
        transform,
        &backdrop.pixels,
        backdrop.width,
        backdrop.height,
    )
    .expect("rasterise");
    Bitmap {
        width: backdrop.width,
        height: backdrop.height,
        pixels,
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
}

fn one_way(a: &Bitmap, b: &Bitmap, radius: i32, tolerance: i32) -> usize {
    let (w, h) = (a.width as i32, a.height as i32);
    let mut bad = 0;
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
            if (0..3).any(|c| {
                let v = a.pixels[ia + c] as i32;
                v < lo[c] - tolerance || v > hi[c] + tolerance
            }) {
                bad += 1;
            }
        }
    }
    bad
}

pub fn diff(a: &Bitmap, b: &Bitmap, radius: i32, tolerance: i32) -> Diff {
    assert_eq!((a.width, a.height), (b.width, b.height), "bitmap sizes");
    let mut sum = 0u64;
    for (pa, pb) in a
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.pixels.as_chunks::<4>().0)
    {
        sum += (0..3)
            .map(|c| (pa[c] as i64 - pb[c] as i64).unsigned_abs())
            .sum::<u64>();
    }
    Diff {
        bad: one_way(a, b, radius, tolerance) + one_way(b, a, radius, tolerance),
        mean: sum as f64 / (a.pixels.len() as f64 * 0.75),
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

/// Number of pixels darker than `level` in every channel.
pub fn dark_pixels(a: &Bitmap, level: u8) -> usize {
    a.pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] < level && p[1] < level && p[2] < level)
        .count()
}

/// Writes a bitmap as a PNG under the test temp directory, for looking at a failure.
pub fn dump(bitmap: &Bitmap, name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("pl-annot");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.png"));
    let pixmap = pl_draw::pixmap_from_rgba(&bitmap.pixels, bitmap.width, bitmap.height).unwrap();
    pixmap.save_png(&path).unwrap();
    path
}

/// Where a render differs from the backdrop by more than 60 grey levels: the pixel box of the
/// differing pixels and how many there are. `None` when nothing differs.
pub fn ink(image: &Bitmap, backdrop: &Bitmap) -> Option<([u32; 4], usize)> {
    let mut found: Option<[u32; 4]> = None;
    let mut count = 0;
    for y in 0..image.height {
        for x in 0..image.width {
            let (a, b) = (image.pixel(x, y), backdrop.pixel(x, y));
            if (0..3).any(|c| (a[c] as i32 - b[c] as i32).abs() > 60) {
                count += 1;
                found = Some(match found {
                    None => [x, y, x, y],
                    Some([x0, y0, x1, y1]) => [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
                });
            }
        }
    }
    found.map(|b| (b, count))
}

/// Compares where two renders put their ink over the same backdrop: the boxes must agree
/// within `units` page units on every side and the amounts within a third. This is the
/// comparison for anything with text in it, where PDFium draws its own substitute for
/// Helvetica and glyph shapes differ. Returns a description of the disagreement.
pub fn ink_mismatch(
    pdfium: &Bitmap,
    ours: &Bitmap,
    backdrop: &Bitmap,
    scale: f64,
    units: f64,
) -> Option<String> {
    let (Some((p, pn)), Some((o, on))) = (ink(pdfium, backdrop), ink(ours, backdrop)) else {
        return Some("nothing was drawn in one of the renders".into());
    };
    let tol = (units * scale).ceil() as i64;
    let off = (0..4).any(|i| (p[i] as i64 - o[i] as i64).abs() > tol);
    let ratio = on as f64 / pn as f64;
    (off || !(0.67..=1.5).contains(&ratio))
        .then(|| format!("pdfium {p:?} ({pn} px) ours {o:?} ({on} px) ratio {ratio:.2}"))
}

/// The characters PDFium finds in the content of one page.
pub fn page_text(
    backend: &PdfiumBackend,
    bytes: Vec<u8>,
    page: usize,
    geometry: &PageGeometry,
) -> pl_render::PageText {
    let doc = backend
        .open(DocSource::Bytes(bytes), None)
        .expect("PDFium opens the file")
        .doc;
    backend
        .set_page_user_unit(doc, page as u32, geometry.user_unit())
        .unwrap();
    let text = backend.page_text(doc, page as u32).unwrap();
    backend.close(doc).unwrap();
    text
}

/// Makes an appearance stream the content of its page (in place of what the page had), so
/// that PDFium's text extraction reads the appearance: the stream is in page coordinates with
/// an identity matrix, so its text lands exactly where the annotation shows it.
pub fn flatten_onto_page(work: &mut Work, page: usize, appearance: ObjectId) {
    let doc = work.doc().clone();
    let page_id = doc.page_id(page).unwrap();
    let Some(pl_pdf::Object::Stream(stream)) = doc.object(appearance) else {
        panic!("the appearance is a stream");
    };
    let resources = stream.dict.get(b"Resources").unwrap().clone();
    let mut edits = doc.edit();
    edits
        .patch(page_id)
        .set("Contents", pl_pdf::Object::Reference(appearance))
        .set("Resources", resources);
    work.file
        .save_incremental(&edits, &SaveOptions::default())
        .expect("save the flattened page");
}
