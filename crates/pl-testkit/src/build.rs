//! Low-level deterministic PDF building helpers on top of lopdf.

use crate::{Rect, Segment, TextTruth};
use lopdf::xref::XrefType;
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, SaveOptions, Stream, StringFormat};

/// Format a number for a content stream: at most 4 decimals, no trailing zeros.
///
/// Fixture coordinates are chosen so that parsing the text gives back the
/// exact `f64` stored in the manifest.
pub(crate) fn num(v: f64) -> String {
    let mut s = format!("{v:.4}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" {
        s = "0".into();
    }
    s
}

/// A PDF number object: integer when integral, real otherwise.
pub(crate) fn number(v: f64) -> Object {
    if v.fract() == 0.0 && v.abs() < 1e9 {
        Object::Integer(v as i64)
    } else {
        Object::Real(v as f32)
    }
}

pub(crate) fn array(values: &[f64]) -> Object {
    Object::Array(values.iter().map(|v| number(*v)).collect())
}

pub(crate) fn name(n: &str) -> Object {
    Object::Name(n.as_bytes().to_vec())
}

pub(crate) fn text(s: &str) -> Object {
    Object::String(s.as_bytes().to_vec(), StringFormat::Literal)
}

/// Escape a string for use inside `( )` in a content stream.
pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        assert!(c.is_ascii() && !c.is_ascii_control(), "fixture text must be printable ASCII");
        if matches!(c, '(' | ')' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Content-stream text builder.
#[derive(Default)]
pub(crate) struct Content(pub String);

impl Content {
    pub fn raw(&mut self, s: &str) -> &mut Self {
        self.0.push_str(s);
        self.0.push('\n');
        self
    }

    /// One stroked path per segment: `w RG m l S`.
    pub fn segment(&mut self, s: &Segment) -> &mut Self {
        let line = format!(
            "{} w {} {} {} RG {} {} m {} {} l S",
            num(s.width),
            num(s.rgb[0]),
            num(s.rgb[1]),
            num(s.rgb[2]),
            num(s.a[0]),
            num(s.a[1]),
            num(s.b[0]),
            num(s.b[1]),
        );
        self.raw(&line)
    }

    pub fn segments(&mut self, segs: &[Segment]) -> &mut Self {
        for s in segs {
            self.segment(s);
        }
        self
    }

    /// One `BT .. ET` block per string, black fill, font resource `font`.
    pub fn text(&mut self, font: &str, t: &TextTruth) -> &mut Self {
        let line = format!(
            "BT /{} {} Tf 0 g {} {} Td ({}) Tj ET",
            font,
            num(t.font_size),
            num(t.origin[0]),
            num(t.origin[1]),
            escape(&t.text)
        );
        self.raw(&line)
    }

    pub fn texts(&mut self, texts: &[TextTruth]) -> &mut Self {
        for t in texts {
            self.text(PAGE_FONT, t);
        }
        self
    }

    pub fn into_stream(self) -> Stream {
        Stream::new(Dictionary::new(), self.0.into_bytes())
    }
}

/// Resource name of the page font (standard-14 Helvetica, WinAnsi).
pub(crate) const PAGE_FONT: &str = "F1";

pub(crate) struct PageSpec {
    pub media_box: Rect,
    pub crop_box: Option<Rect>,
    pub rotate: i32,
    pub user_unit: f64,
}

#[derive(Clone, Copy)]
pub(crate) enum SaveKind {
    /// Classic `xref` table and `trailer`.
    Classic,
    /// Cross-reference stream plus object streams.
    Modern,
}

pub(crate) struct Builder {
    pub doc: Document,
    pub font_id: ObjectId,
    /// Extra catalog entries (Outlines, OCProperties, AcroForm, ...).
    pub catalog: Dictionary,
    pages_id: ObjectId,
    kids: Vec<ObjectId>,
}

impl Builder {
    pub fn new(version: &str) -> Self {
        let mut doc = Document::with_version(version);
        doc.reference_table.cross_reference_type = XrefType::CrossReferenceTable;
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => name("Font"),
            "Subtype" => name("Type1"),
            "BaseFont" => name("Helvetica"),
            "Encoding" => name("WinAnsiEncoding"),
        });
        Self { doc, font_id, catalog: Dictionary::new(), pages_id, kids: Vec::new() }
    }

    /// Add a page whose `/Contents` is the given streams (an array when more than one).
    /// `extra_resources` entries are added beside `/Font << /F1 >>`.
    pub fn add_page(&mut self, spec: &PageSpec, contents: Vec<Stream>, extra_resources: Dictionary) -> ObjectId {
        let content_ids: Vec<Object> =
            contents.into_iter().map(|s| Object::Reference(self.doc.add_object(s))).collect();
        let mut resources = dictionary! {
            "Font" => dictionary! { PAGE_FONT => Object::Reference(self.font_id) },
        };
        for (k, v) in extra_resources.iter() {
            resources.set(k.clone(), v.clone());
        }
        let mut page = dictionary! {
            "Type" => name("Page"),
            "Parent" => Object::Reference(self.pages_id),
            "MediaBox" => array(&spec.media_box),
        };
        if let Some(crop) = spec.crop_box {
            page.set("CropBox", array(&crop));
        }
        if spec.rotate != 0 {
            page.set("Rotate", Object::Integer(i64::from(spec.rotate)));
        }
        if spec.user_unit != 1.0 {
            page.set("UserUnit", number(spec.user_unit));
        }
        page.set("Resources", resources);
        if content_ids.len() == 1 {
            page.set("Contents", content_ids[0].clone());
        } else {
            page.set("Contents", Object::Array(content_ids));
        }
        let id = self.doc.add_object(page);
        self.kids.push(id);
        id
    }

    pub fn set_annots(&mut self, page_id: ObjectId, annots: &[ObjectId]) {
        let refs = annots.iter().map(|id| Object::Reference(*id)).collect();
        self.doc
            .get_dictionary_mut(page_id)
            .expect("page exists")
            .set("Annots", Object::Array(refs));
    }

    /// Finish the page tree, catalog, info and trailer. No timestamps are written.
    pub fn finish(mut self, fixture_name: &str) -> Document {
        let kids: Vec<Object> = self.kids.iter().map(|id| Object::Reference(*id)).collect();
        let count = kids.len() as i64;
        self.doc.objects.insert(
            self.pages_id,
            Object::Dictionary(dictionary! {
                "Type" => name("Pages"),
                "Kids" => Object::Array(kids),
                "Count" => Object::Integer(count),
            }),
        );
        let mut catalog = dictionary! {
            "Type" => name("Catalog"),
            "Pages" => Object::Reference(self.pages_id),
        };
        for (k, v) in self.catalog.iter() {
            catalog.set(k.clone(), v.clone());
        }
        let catalog_id = self.doc.add_object(catalog);
        let info_id = self.doc.add_object(dictionary! {
            "Title" => text(&format!("pl-testkit fixture: {fixture_name}")),
            "Producer" => text("pl-testkit"),
        });
        let id = Object::String(file_id(fixture_name).to_vec(), StringFormat::Hexadecimal);
        self.doc.trailer.set("Root", Object::Reference(catalog_id));
        self.doc.trailer.set("Info", Object::Reference(info_id));
        self.doc.trailer.set("ID", Object::Array(vec![id.clone(), id]));
        self.doc
    }
}

pub(crate) fn save(mut doc: Document, kind: SaveKind) -> Vec<u8> {
    let mut out = Vec::new();
    match kind {
        SaveKind::Classic => {
            doc.reference_table.cross_reference_type = XrefType::CrossReferenceTable;
            doc.save_to(&mut out).expect("writing to a Vec cannot fail");
        }
        SaveKind::Modern => {
            // Not the builder: its default compression level is 0, which leaves
            // object streams uncompressed. `Default` gives Flate level 6.
            let options =
                SaveOptions { use_object_streams: true, use_xref_streams: true, ..SaveOptions::default() };
            doc.save_with_options(&mut out, options).expect("writing to a Vec cannot fail");
        }
    }
    out
}

/// Deterministic 16-byte file identifier derived from the fixture name (FNV-1a).
pub(crate) fn file_id(fixture_name: &str) -> [u8; 16] {
    fn fnv(seed: u64, data: &[u8]) -> u64 {
        let mut h = seed;
        for b in data {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }
    let a = fnv(0xcbf2_9ce4_8422_2325, fixture_name.as_bytes());
    let b = fnv(a ^ 0x9e37_79b9_7f4a_7c15, fixture_name.as_bytes());
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&a.to_be_bytes());
    out[8..].copy_from_slice(&b.to_be_bytes());
    out
}

/// Put the pages `kids` under a new intermediate `/Pages` node that carries `inherited`, and
/// take the keys in `strip` off the pages themselves, so the pages inherit those attributes.
/// The pages must be consecutive children of the same parent.
pub(crate) fn group_under_node(
    doc: &mut Document,
    kids: &[ObjectId],
    inherited: Dictionary,
    strip: &[&str],
) -> ObjectId {
    let parent = doc
        .get_dictionary(kids[0])
        .and_then(|d| d.get(b"Parent"))
        .and_then(Object::as_reference)
        .expect("page has a /Parent");
    let mut node = dictionary! {
        "Type" => name("Pages"),
        "Parent" => Object::Reference(parent),
        "Kids" => Object::Array(kids.iter().map(|id| Object::Reference(*id)).collect()),
        "Count" => Object::Integer(kids.len() as i64),
    };
    for (k, v) in inherited.iter() {
        node.set(k.clone(), v.clone());
    }
    let node_id = doc.add_object(node);
    for kid in kids {
        let page = doc.get_dictionary_mut(*kid).expect("page exists");
        page.set("Parent", Object::Reference(node_id));
        for key in strip {
            page.remove(key.as_bytes());
        }
    }
    let parent_kids = doc
        .get_dictionary_mut(parent)
        .and_then(|d| d.get_mut(b"Kids"))
        .and_then(Object::as_array_mut)
        .expect("parent has /Kids");
    let first = parent_kids
        .iter()
        .position(|o| matches!(o, Object::Reference(id) if *id == kids[0]))
        .expect("first kid is in the parent's /Kids");
    parent_kids.retain(|o| !matches!(o, Object::Reference(id) if kids.contains(id)));
    parent_kids.insert(first, Object::Reference(node_id));
    node_id
}

/// A form XObject (appearance stream) with the given bounding box and resources.
pub(crate) fn form_xobject(bbox: Rect, resources: Dictionary, content: String) -> Stream {
    Stream::new(
        dictionary! {
            "Type" => name("XObject"),
            "Subtype" => name("Form"),
            "FormType" => Object::Integer(1),
            "BBox" => array(&bbox),
            "Resources" => resources,
        },
        content.into_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_formatting() {
        assert_eq!(num(72.0), "72");
        assert_eq!(num(0.25), "0.25");
        assert_eq!(num(-0.0), "0");
        assert_eq!(num(100.5), "100.5");
        assert_eq!(num(1.23456), "1.2346");
        assert_eq!(num(1000.0), "1000");
    }

    #[test]
    fn escaping() {
        assert_eq!(escape("a(b)c\\"), "a\\(b\\)c\\\\");
    }

    #[test]
    fn file_ids_differ() {
        assert_ne!(file_id("a"), file_id("b"));
        assert_eq!(file_id("a"), file_id("a"));
    }
}
