//! Fixtures written byte by byte, without lopdf's writer: the linearised file (lopdf cannot
//! produce that layout) and the large scan set (streamed to disk, never held in memory).

use crate::build::{file_id, Content, SaveKind, PAGE_FONT};
use crate::fixtures::{manifest, plain_truth};
use crate::{Fixture, PageTruth, Segment, TextTruth, BLACK, LETTER, RED};
use std::io::{self, BufWriter, Write};
use std::path::Path;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

fn page_content(truth: &mut PageTruth) -> Vec<u8> {
    let mut c = Content::default();
    c.segments(&truth.segments).texts(&truth.texts);
    truth.segment_count = truth.segments.len();
    truth.content_streams = 1;
    c.0.into_bytes()
}

/// Offsets found while assembling the linearised file; fed back in for the second pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct LinLayout {
    /// Offset of object `n` at index `n` (index 0 unused).
    objects: [u64; 11],
    first_xref: u64,
    main_xref: u64,
    /// Offset of the first entry of the main cross-reference table (`/T`).
    main_first_entry: u64,
    hint: (u64, u64),
    first_page_end: u64,
    len: u64,
}

/// Object numbers of the linearised fixture. The main section holds 1 to 5, the first-page
/// section 6 to 10, as a lineariser numbers them.
mod lin {
    pub const PAGES: u32 = 1;
    pub const PAGE2: u32 = 2;
    pub const CONTENT2: u32 = 3;
    pub const FONT: u32 = 4;
    pub const INFO: u32 = 5;
    pub const PARAMS: u32 = 6;
    pub const CATALOG: u32 = 7;
    pub const HINT: u32 = 8;
    pub const PAGE1: u32 = 9;
    pub const CONTENT1: u32 = 10;
}

fn assemble_linearized(known: &LinLayout, contents: [&[u8]; 2]) -> (Vec<u8>, LinLayout) {
    let mut out: Vec<u8> = Vec::new();
    let mut found = LinLayout::default();
    let id = hex(&file_id("linearized"));
    let trailer_common = format!(
        "/Root {} 0 R /Info {} 0 R /ID [<{id}> <{id}>]",
        lin::CATALOG,
        lin::INFO
    );
    let begin = |out: &mut Vec<u8>, found: &mut LinLayout, n: u32| {
        found.objects[n as usize] = out.len() as u64;
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
    };
    let page = |content: u32| {
        format!(
            "<< /Type /Page /Parent {} 0 R /MediaBox [0 0 612 792] /Resources << /Font << /{PAGE_FONT} {} 0 R >> >> /Contents {content} 0 R >>\nendobj\n",
            lin::PAGES,
            lin::FONT
        )
    };
    let stream = |out: &mut Vec<u8>, extra: &str, data: &[u8]| {
        out.extend_from_slice(format!("<< /Length {}{extra} >>\nstream\n", data.len()).as_bytes());
        out.extend_from_slice(data);
        out.extend_from_slice(b"\nendstream\nendobj\n");
    };
    let xref_entry = |offset: u64| format!("{offset:010} 00000 n \n");

    out.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");

    // Part 1: the linearisation parameter dictionary. Every number is ten digits wide so the
    // layout is the same on both passes.
    begin(&mut out, &mut found, lin::PARAMS);
    out.extend_from_slice(
        format!(
            "<< /Linearized 1 /L {:010} /H [ {:010} {:010} ] /O {} /E {:010} /N 2 /T {:010} >>\nendobj\n",
            known.len, known.hint.0, known.hint.1, lin::PAGE1, known.first_page_end, known.main_first_entry
        )
        .as_bytes(),
    );

    // Part 2: the first-page cross-reference table and trailer.
    found.first_xref = out.len() as u64;
    out.extend_from_slice(format!("xref\n{} 5\n", lin::PARAMS).as_bytes());
    for n in lin::PARAMS..=lin::CONTENT1 {
        out.extend_from_slice(xref_entry(known.objects[n as usize]).as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size 11 {trailer_common} /Prev {:010} >>\nstartxref\n0\n%%EOF\n",
            known.main_xref
        )
        .as_bytes(),
    );

    // Parts 3 to 6: catalog, hint stream, first-page objects.
    begin(&mut out, &mut found, lin::CATALOG);
    out.extend_from_slice(format!("<< /Type /Catalog /Pages {} 0 R >>\nendobj\n", lin::PAGES).as_bytes());
    found.hint.0 = out.len() as u64;
    begin(&mut out, &mut found, lin::HINT);
    // A placeholder with the shape of a hint stream: a page-offset table header and a
    // shared-object table header, all zero. Nothing in this workspace reads hint tables; a
    // reader that does must treat them as absent.
    stream(&mut out, " /S 36", &[0u8; 60]);
    found.hint.1 = out.len() as u64 - found.hint.0;
    begin(&mut out, &mut found, lin::PAGE1);
    out.extend_from_slice(page(lin::CONTENT1).as_bytes());
    begin(&mut out, &mut found, lin::CONTENT1);
    stream(&mut out, "", contents[0]);
    found.first_page_end = out.len() as u64;

    // Parts 7 to 9: everything else.
    begin(&mut out, &mut found, lin::PAGE2);
    out.extend_from_slice(page(lin::CONTENT2).as_bytes());
    begin(&mut out, &mut found, lin::CONTENT2);
    stream(&mut out, "", contents[1]);
    begin(&mut out, &mut found, lin::PAGES);
    out.extend_from_slice(
        format!(
            "<< /Type /Pages /Kids [{} 0 R {} 0 R] /Count 2 >>\nendobj\n",
            lin::PAGE1,
            lin::PAGE2
        )
        .as_bytes(),
    );
    begin(&mut out, &mut found, lin::FONT);
    out.extend_from_slice(
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>\nendobj\n",
    );
    begin(&mut out, &mut found, lin::INFO);
    out.extend_from_slice(
        b"<< /Title (pl-testkit fixture: linearized) /Producer (pl-testkit) >>\nendobj\n",
    );

    // Part 11: the main cross-reference table, then `startxref` pointing back at part 2.
    found.main_xref = out.len() as u64;
    out.extend_from_slice(b"xref\n0 6\n");
    found.main_first_entry = out.len() as u64;
    out.extend_from_slice(b"0000000000 65535 f \n");
    for n in lin::PAGES..=lin::INFO {
        out.extend_from_slice(xref_entry(known.objects[n as usize]).as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size 6 {trailer_common} >>\nstartxref\n{}\n%%EOF\n",
            known.first_xref
        )
        .as_bytes(),
    );
    found.len = out.len() as u64;
    (out, found)
}

/// Two Letter pages in the linearised ("fast web view") layout: the `/Linearized` parameter
/// dictionary is the first object, the first-page cross-reference table follows it and chains
/// through `/Prev` to the main table at the end of the file, and the final `startxref` points
/// back at the first-page table. `/L`, `/O`, `/E`, `/N` and `/T` are correct; the hint stream
/// named by `/H` is a zero-filled placeholder (see the source), so this is a structural
/// stand-in for a lineariser's output, enough for parsers and for incremental save.
///
/// Page 1 has the [`crate::plain`] marks plus a label; page 2 two lines and a label.
pub fn linearized() -> Fixture {
    let mut p1 = plain_truth();
    p1.texts.push(TextTruth::new("LINEARIZED page 1", [72.0, 740.0], 12.0));
    let mut p2 = PageTruth::new(LETTER);
    p2.segments = vec![
        Segment::new([72.0, 72.0], [540.0, 72.0], 1.0, BLACK),
        Segment::new([72.0, 200.0], [272.0, 200.0], 2.0, RED),
    ];
    p2.texts.push(TextTruth::new("LINEARIZED page 2", [72.0, 740.0], 12.0));
    let contents = [page_content(&mut p1), page_content(&mut p2)];
    let contents = [contents[0].as_slice(), contents[1].as_slice()];

    // Every offset inside the file is written ten digits wide, so one pass finds them all. Only
    // the closing `startxref` value is written at its natural width, which moves nothing but
    // the file length; a second pass settles that.
    let (_, first) = assemble_linearized(&LinLayout::default(), contents);
    let (_, layout) = assemble_linearized(&first, contents);
    let (bytes, again) = assemble_linearized(&layout, contents);
    assert_eq!(layout, again, "the layout is a fixed point");
    assert_eq!(bytes.len() as u64, layout.len);

    let mut manifest = manifest("linearized", "1.4", SaveKind::Classic, vec![p1, p2]);
    manifest.linearized = true;
    Fixture { manifest, bytes }
}

/// What [`write_scan_set`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanSet {
    pub pages: usize,
    pub image_px: [u32; 2],
    /// Length of the file in bytes.
    pub file_len: u64,
}

/// Write a stand-in for a set of scanned sheets straight to `path`: `pages` Letter pages, each
/// fully covered by one 8-bit DeviceGray image of `image_px` pixels holding pseudo-random
/// noise, stored without a filter. Like a real scan (JPEG or CCITT data), the image bytes do
/// not compress and mean nothing to a parser; what this exercises is file size. 36 pages of
/// 2550 x 3300 px are about 300 MB.
///
/// The file is streamed, so generating it needs one image row of memory, not the file size.
/// It is deterministic for given arguments.
pub fn write_scan_set(path: &Path, pages: usize, image_px: [u32; 2]) -> io::Result<ScanSet> {
    write_scan_set_with_unit(path, pages, image_px, 1.0)
}

/// As [`write_scan_set`], with `/UserUnit` set on every page (left out when it is 1). The
/// MediaBox stays `[0 0 612 792]`, so with a unit of 2 each sheet is 17 x 22 inches. This is
/// the large file on which finding `/UserUnit` must not cost a full parse.
pub fn write_scan_set_with_unit(
    path: &Path,
    pages: usize,
    image_px: [u32; 2],
    user_unit: f64,
) -> io::Result<ScanSet> {
    assert!(pages >= 1 && image_px[0] >= 1 && image_px[1] >= 1);
    let unit = if user_unit == 1.0 {
        String::new()
    } else {
        format!(" /UserUnit {}", crate::build::num(user_unit))
    };
    let mut w = Counting { inner: BufWriter::with_capacity(1 << 20, std::fs::File::create(path)?), pos: 0 };
    // Objects: 1 catalog, 2 pages, 3 font, then per page i: page 4+3i, content 5+3i, image 6+3i.
    let total = 3 + 3 * pages as u32;
    let mut offsets = vec![0u64; total as usize + 1];
    w.write_all(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n")?;

    offsets[1] = w.pos;
    w.write_all(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n")?;
    offsets[2] = w.pos;
    let kids: String = (0..pages as u32).map(|i| format!("{} 0 R ", 4 + 3 * i)).collect();
    w.write_all(format!("2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count {pages} >>\nendobj\n").as_bytes())?;
    offsets[3] = w.pos;
    w.write_all(
        b"3 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>\nendobj\n",
    )?;

    let [width, height] = image_px;
    let mut row = vec![0u8; width as usize];
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    for i in 0..pages as u32 {
        let (page, content, image) = (4 + 3 * i, 5 + 3 * i, 6 + 3 * i);
        offsets[page as usize] = w.pos;
        w.write_all(
            format!(
                "{page} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]{unit} /Resources << /Font << /{PAGE_FONT} 3 0 R >> /XObject << /Im0 {image} 0 R >> >> /Contents {content} 0 R >>\nendobj\n"
            )
            .as_bytes(),
        )?;
        offsets[content as usize] = w.pos;
        let label = format!("SCAN-SET sheet {}", i + 1);
        let ops = format!("q 612 0 0 792 0 0 cm /Im0 Do Q\nBT /{PAGE_FONT} 12 Tf 0 g 72 740 Td ({label}) Tj ET\n");
        w.write_all(format!("{content} 0 obj\n<< /Length {} >>\nstream\n{ops}\nendstream\nendobj\n", ops.len()).as_bytes())?;
        offsets[image as usize] = w.pos;
        let data_len = u64::from(width) * u64::from(height);
        w.write_all(
            format!(
                "{image} 0 obj\n<< /Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace /DeviceGray /BitsPerComponent 8 /Length {data_len} >>\nstream\n"
            )
            .as_bytes(),
        )?;
        for _ in 0..height {
            for chunk in row.chunks_mut(8) {
                // xorshift64*: fast, and good enough to defeat any compressor.
                state ^= state >> 12;
                state ^= state << 25;
                state ^= state >> 27;
                let v = state.wrapping_mul(0x2545_f491_4f6c_dd1d).to_le_bytes();
                chunk.copy_from_slice(&v[..chunk.len()]);
            }
            w.write_all(&row)?;
        }
        w.write_all(b"\nendstream\nendobj\n")?;
    }

    let xref = w.pos;
    w.write_all(format!("xref\n0 {}\n0000000000 65535 f \n", total + 1).as_bytes())?;
    for offset in &offsets[1..] {
        w.write_all(format!("{offset:010} 00000 n \n").as_bytes())?;
    }
    let id = hex(&file_id("scan_set"));
    w.write_all(
        format!("trailer\n<< /Size {} /Root 1 0 R /ID [<{id}> <{id}>] >>\nstartxref\n{xref}\n%%EOF\n", total + 1)
            .as_bytes(),
    )?;
    w.inner.flush()?;
    Ok(ScanSet { pages, image_px, file_len: w.pos })
}

struct Counting<W: Write> {
    inner: W,
    pos: u64,
}

impl<W: Write> Counting<W> {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.inner.write_all(bytes)?;
        self.pos += bytes.len() as u64;
        Ok(())
    }
}
