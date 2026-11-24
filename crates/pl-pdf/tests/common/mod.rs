//! Shared helpers for the integration tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use pl_pdf::{Dictionary, EditSet, Object, ObjectId, PdfDocument, StringFormat};
use pl_testkit::{Fixture, Options};

/// Every fixture at test size (2,000-segment sheet, small raster).
pub fn fixtures() -> Vec<Fixture> {
    pl_testkit::all_fixtures_with(&Options::small())
}

pub fn write_fixture(dir: &Path, fixture: &Fixture) -> PathBuf {
    let path = dir.join(fixture.manifest.file_name());
    std::fs::write(&path, &fixture.bytes).unwrap();
    path
}

pub const SQUARE_CONTENTS: &str = "Plumbline test square";
pub const SQUARE_AP: &[u8] = b"q 1 0 0 RG 2 w 1 1 98 48 re S Q";
pub const SQUARE_RECT: [i64; 4] = [100, 100, 200, 150];

fn name(s: &str) -> Object {
    Object::Name(s.as_bytes().to_vec())
}

fn text(s: &str) -> Object {
    Object::String(s.as_bytes().to_vec(), StringFormat::Literal)
}

fn ints(v: &[i64]) -> Object {
    Object::Array(v.iter().map(|i| Object::Integer(*i)).collect())
}

/// Add a Square annotation with a hand-built `/AP /N` form XObject to `page_index`.
/// Returns (annotation id, appearance stream id).
pub fn add_square(
    edits: &mut EditSet,
    doc: &PdfDocument,
    page_index: usize,
) -> (ObjectId, ObjectId) {
    let mut ap = Dictionary::new();
    ap.set("Type", name("XObject"));
    ap.set("Subtype", name("Form"));
    ap.set("BBox", ints(&[0, 0, 100, 50]));
    ap.set("Resources", Object::Dictionary(Dictionary::new()));
    let ap_id = edits.add_stream(ap, SQUARE_AP.to_vec());

    let mut n = Dictionary::new();
    n.set("N", Object::Reference(ap_id));

    let mut annot = Dictionary::new();
    annot.set("Type", name("Annot"));
    annot.set("Subtype", name("Square"));
    annot.set("Rect", ints(&SQUARE_RECT));
    annot.set("C", ints(&[1, 0, 0]));
    annot.set("F", Object::Integer(4));
    annot.set("NM", text("pl-test-0001"));
    annot.set("T", text("Tester"));
    annot.set("Contents", text(SQUARE_CONTENTS));
    annot.set("P", Object::Reference(doc.page_id(page_index).unwrap()));
    annot.set("AP", Object::Dictionary(n));
    let id = edits.add_annotation(page_index, annot);
    (id, ap_id)
}

pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
