//! Writing: turning the difference between the model and the file into a pl-pdf edit set.
//!
//! * A new annotation gets a complete dictionary and a generated `/AP /N` stream.
//! * A changed annotation gets a patch that names only the keys whose fields changed; pl-pdf
//!   applies it to the dictionary as the file holds it, so every other key (unknown and vendor
//!   keys included) is carried over untouched.
//! * An unchanged annotation is not mentioned at all, so its object is never rewritten.
//! * A removed annotation is taken out of its page's `/Annots`.

use std::collections::{BTreeSet, HashMap, HashSet};

use pl_draw::emit::{Appearance, TransparencyGroup, XObject};
use pl_draw::{DrawError, Rgb};
use pl_geom::{Point, Rect, Vec2};
use pl_pdf::{DictPatch, Dictionary, EditSet, Object, ObjectId, PdfDocument, Stream, StringFormat};

use crate::appearance::Placement;
use crate::content::{default_appearance, default_style, rich_text, FreeText, StampContent};
use crate::measure::{CaptionPosition, CountSymbol, MeasureKind};
use crate::measure_io::{measure_key, MEASURE_KEYS};
use crate::model::{
    AnnotId, AnnotKind, Annotation, Baseline, Geometry, LinkTarget, COLUMN_DATA_KEY, MODEL_KEY,
    PRIVATE_KEY,
};
use crate::objects::{decode_text, encode_text, lookup, name, name_of, num, nums};
use crate::read::quad_to_file;
use crate::set::AnnotSet;

/// Two coordinates closer than this are the same for the purpose of "did this field change".
/// It is above the resolution of the single-precision numbers a PDF holds on the largest page
/// (14,400 units), so a model that has only been through a save and a load is never dirty.
const SAME: f64 = 0.002;

#[derive(Debug, thiserror::Error)]
pub enum AnnotError {
    #[error("annotation {id} is on page index {page}, but the document has {count} pages")]
    PageOutOfRange {
        id: AnnotId,
        page: usize,
        count: usize,
    },

    #[error("two annotations share the id {0}")]
    DuplicateId(AnnotId),

    #[error(
        "annotation {0} is written directly inside its page's /Annots array, so it cannot be \
         changed or deleted by an incremental edit"
    )]
    NotEditable(AnnotId),

    #[error(
        "annotation {0} was read from another state of the document than the one being saved; \
         reload the annotations"
    )]
    Stale(AnnotId),

    #[error(
        "annotation {0} is new but of a kind (or with a geometry) this version cannot write;          only modelled kinds can be added"
    )]
    Unmodelled(AnnotId),

    #[error("annotation {0} has a coordinate that is not a finite number")]
    NotFinite(AnnotId),

    #[error("the appearance of annotation {id} could not be generated: {source}")]
    Draw {
        id: AnnotId,
        #[source]
        source: DrawError,
    },
}

/// One annotation a save writes.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub id: AnnotId,
    pub page: usize,
    /// The annotation's object (new for an added annotation).
    pub object: ObjectId,
    /// The dictionary keys written or removed, sorted. For an added annotation, every key of
    /// its dictionary.
    pub keys: Vec<String>,
    /// The new `/AP /N` stream, when one was generated.
    pub appearance: Option<ObjectId>,
    /// For a count: the further annotations it is written as, one per marker after the first
    /// (new objects; the ones the file had are taken off the page).
    pub members: Vec<ObjectId>,
}

/// One annotation a save removes from its page.
#[derive(Debug, Clone, PartialEq)]
pub struct Deletion {
    pub id: AnnotId,
    pub page: usize,
    pub object: ObjectId,
}

/// What a save of an [`AnnotSet`] will do, with the pl-pdf edit set that does it.
#[derive(Debug, Clone, PartialEq)]
pub struct SavePlan {
    /// Pass this to `PdfFile::save_incremental` (or `save_full`).
    pub edits: EditSet,
    pub added: Vec<Change>,
    pub modified: Vec<Change>,
    pub deleted: Vec<Deletion>,
    /// Pages whose `/Annots` order is rewritten because the stacking order changed.
    pub reordered_pages: Vec<usize>,
}

impl SavePlan {
    /// True when the set matches the file and there is nothing to save.
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    /// Indices of the pages whose look or annotation list the save changes, ascending.
    pub fn touched_pages(&self) -> Vec<usize> {
        let mut pages: BTreeSet<usize> = self.reordered_pages.iter().copied().collect();
        pages.extend(self.added.iter().map(|c| c.page));
        pages.extend(self.modified.iter().map(|c| c.page));
        pages.extend(self.deleted.iter().map(|r| r.page));
        pages.extend(self.edits.annotation_pages());
        pages.into_iter().collect()
    }
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() <= SAME
}

fn near_point(a: Point, b: Point) -> bool {
    near(a.x, b.x) && near(a.y, b.y)
}

fn near_rect(a: Rect, b: Rect) -> bool {
    near(a.x0, b.x0) && near(a.y0, b.y0) && near(a.x1, b.x1) && near(a.y1, b.y1)
}

fn near_points(a: &[Point], b: &[Point]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(p, q)| near_point(*p, *q))
}

fn near_geometry(a: &Geometry, b: &Geometry) -> bool {
    match (a, b) {
        (Geometry::Rect(a), Geometry::Rect(b)) => near_rect(*a, *b),
        (Geometry::Line { a: a0, b: a1 }, Geometry::Line { a: b0, b: b1 }) => {
            near_point(*a0, *b0) && near_point(*a1, *b1)
        }
        (Geometry::Points(a), Geometry::Points(b)) => near_points(a, b),
        (Geometry::Ink(a), Geometry::Ink(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(s, t)| near_points(s, t))
        }
        (Geometry::Quads(a), Geometry::Quads(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(s, t)| near_points(s, t))
        }
        _ => false,
    }
}

fn near_color(a: Option<Rgb>, b: Option<Rgb>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            let c = |x: f32, y: f32| (x - y).abs() <= 1.0 / 1024.0;
            c(a.r, b.r) && c(a.g, b.g) && c(a.b, b.b)
        }
        _ => false,
    }
}

fn near_dash(a: &Option<Vec<f64>>, b: &Option<Vec<f64>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| near(*x, *y)),
        _ => false,
    }
}

/// The style keys a kind uses. A style field outside this list is not written for the kind.
fn style_keys(kind: &AnnotKind) -> &'static [&'static str] {
    match kind {
        AnnotKind::Square | AnnotKind::Circle | AnnotKind::Polygon => {
            &["C", "IC", "CA", "BS", "BE", "BM"]
        }
        AnnotKind::Line | AnnotKind::PolyLine => &["C", "IC", "CA", "BS", "LE", "BM"],
        AnnotKind::Ink => &["C", "CA", "BS", "BM"],
        AnnotKind::Highlight
        | AnnotKind::Underline
        | AnnotKind::StrikeOut
        | AnnotKind::Squiggly
        | AnnotKind::Text => &["C", "CA", "BM"],
        AnnotKind::Link => &["C", "BS"],
        // For a FreeText /C is the background; the border colour is in /DA.
        AnnotKind::FreeText => &["C", "CA", "BS", "BE"],
        AnnotKind::Stamp => &["CA"],
        _ => &[],
    }
}

/// Every key that says how a modelled FreeText looks, apart from its text. They are written
/// together: a key whose value comes out as the file has it is left alone by the save.
const FREE_TEXT_KEYS: [&str; 12] = [
    "C",
    "CA",
    "BS",
    "BE",
    "DA",
    "DS",
    "Q",
    "IT",
    "CL",
    "LE",
    "Rotate",
    PRIVATE_KEY,
];

/// The modelled FreeText of `a`, if it is one.
fn free_text_of(a: &Annotation) -> Option<&FreeText> {
    a.free_text
        .as_ref()
        .filter(|_| a.kind == AnnotKind::FreeText && a.is_modelled())
}

/// A number written as a string, so that it reads back exactly.
fn exact(v: f64) -> Object {
    encode_text(&format!("{v}"))
}

/// This crate's own part of the private dictionary: what a FreeText or Stamp of ours holds
/// that no standard key can say.
fn own_model(a: &Annotation) -> Option<Dictionary> {
    if !a.is_modelled() {
        return None;
    }
    if let Some(m) = a.measurement() {
        let mut d = crate::measure_io::measure_model(m);
        match (m.kind, &a.geometry) {
            // A Circle has no key for the line a diameter was drawn along.
            (MeasureKind::Diameter, Geometry::Line { a, b }) => {
                d.set("Line", nums(&[a.x, a.y, b.x, b.y]));
            }
            // The first marker of a count: the count's own annotation.
            (MeasureKind::Count, _) => {
                d.set("Group", encode_text(a.id.as_str()));
                d.set("Item", Object::Integer(0));
                if let Some(p) = a.count_points().first() {
                    d.set("At", crate::measure_io::exact_point_object(*p));
                }
            }
            _ => {}
        }
        if revu_round(a) {
            // Not /LE and /IC: on Revu's Circle and Polygon they would describe the shape.
            d.set(
                "LE",
                Object::Array(vec![
                    name(a.style.line_start.pdf_name()),
                    name(a.style.line_end.pdf_name()),
                ]),
            );
            d.set(
                "IC",
                a.style.fill.map_or(Object::Array(Vec::new()), color_object),
            );
            // Nor /CP, which Revu's radius and diameter do not have.
            if m.caption_position != CaptionPosition::Inline {
                d.set("CP", name(m.caption_position.pdf_name()));
            }
        }
        return Some(d);
    }
    let mut d = Dictionary::new();
    match a.kind {
        AnnotKind::FreeText => {
            let t = a.free_text.as_ref()?;
            d.set("Type", name("FreeText"));
            d.set(
                "VAlign",
                name(match t.v_align {
                    pl_draw::VAlign::Top => "Top",
                    pl_draw::VAlign::Middle => "Middle",
                    pl_draw::VAlign::Bottom => "Bottom",
                }),
            );
            d.set("LineSpacing", exact(t.line_spacing));
            d.set("Padding", exact(t.padding));
            // /DA holds the border colour where Acrobat looks for it and /DS the text colour
            // in eight bits; this is the text colour exactly.
            let c = t.color.clamped();
            d.set(
                "TextColor",
                encode_text(&format!("{} {} {}", c.r, c.g, c.b)),
            );
            if a.style.stroke.is_none() && a.style.width != 0.0 {
                // /BS /W is 0 while there is no border; this is the width it comes back with.
                d.set("W", exact(a.style.width));
            }
        }
        AnnotKind::Stamp => match &a.stamp.as_ref()?.content {
            StampContent::Label(spec) => {
                d.set("Type", name("Stamp"));
                d.set("Name", encode_text(&spec.name));
                d.set("Label", encode_text(&spec.label));
                if let Some(detail) = &spec.detail {
                    d.set("Detail", encode_text(detail));
                }
                d.set("Scheme", name(spec.scheme.name()));
            }
            StampContent::Image(_) => {
                d.set("Type", name("ImageStamp"));
            }
        },
        _ => return None,
    }
    Some(d)
}

/// The value of [`PRIVATE_KEY`]: the application's data with this crate's own part beside it.
fn private_object(a: &Annotation) -> Option<Object> {
    let own = own_model(a);
    if a.app_data.is_none() && own.is_none() {
        return None;
    }
    let mut d = a.app_data.clone().unwrap_or_default();
    if let Some(own) = own {
        d.set(MODEL_KEY, Object::Dictionary(own));
    }
    Some(Object::Dictionary(d))
}

/// The key that holds a modelled kind's own geometry, if it is not `/Rect`.
fn geometry_key(a: &Annotation) -> Option<&'static str> {
    match a.geometry {
        Geometry::Rect(_) => None,
        Geometry::Line { .. } => Some("L"),
        Geometry::Points(_) => Some("Vertices"),
        Geometry::Ink(_) => Some("InkList"),
        Geometry::Quads(_) => Some("QuadPoints"),
    }
}

/// Whether a save gives this annotation an appearance stream of ours.
fn has_generated_appearance(a: &Annotation) -> bool {
    if !a.is_modelled() {
        return false;
    }
    match a.kind {
        AnnotKind::Popup => false,
        // A link is invisible unless it has a border.
        AnnotKind::Link => !a.draw_list().is_empty(),
        _ => true,
    }
}

/// The keys of `current` that differ from the file.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Diff {
    pub keys: BTreeSet<&'static str>,
    /// The appearance has to be generated again.
    pub appearance: bool,
    /// The file's appearance is kept although the geometry or style changed (see [`diff`]);
    /// `/Rect` then follows the geometry so the old appearance moves with it.
    pub keep_appearance: bool,
}

pub(crate) fn diff(current: &Annotation, base: &Baseline) -> Diff {
    let reference = base.reference(current);
    let old = &*reference;
    let mut d = Diff::default();
    let modelled = current.is_modelled();
    let own_content = matches!(current.kind, AnnotKind::FreeText | AnnotKind::Stamp);

    if current.kind != old.kind {
        d.keys.insert("Subtype");
        d.appearance = true;
    }
    if current.page != base.page {
        d.keys.insert("P");
    }
    if !near_geometry(&current.geometry, &old.geometry) {
        match geometry_key(current).filter(|_| modelled) {
            Some(key) => {
                d.keys.insert(key);
            }
            None => {
                d.keys.insert("Rect");
            }
        }
        d.appearance = true;
    }
    let geometry_changed = d.appearance;
    if modelled && !own_content {
        let (s, o) = (&current.style, &old.style);
        for key in style_keys(&current.kind) {
            let same = match *key {
                "C" => near_color(s.stroke, o.stroke),
                "IC" => near_color(s.fill, o.fill),
                "CA" => (s.opacity - o.opacity).abs() <= 1e-4,
                "BS" => near(s.width, o.width) && near_dash(&s.dash, &o.dash),
                "LE" => s.line_start == o.line_start && s.line_end == o.line_end,
                "BE" => match (s.cloud, o.cloud) {
                    (None, None) => true,
                    (Some(a), Some(b)) => near(a, b),
                    _ => false,
                },
                "BM" => s.blend == o.blend,
                _ => true,
            };
            if !same {
                d.keys.insert(key);
                d.appearance = true;
            }
        }
    }

    let text_changed = current.meta.contents != old.meta.contents;
    if let Some(text) = free_text_of(current) {
        let (s, o) = (&current.style, &old.style);
        let same = Some(text) == old.free_text.as_ref()
            && near_color(s.stroke, o.stroke)
            && near_color(s.fill, o.fill)
            && near(s.width, o.width)
            && near_dash(&s.dash, &o.dash)
            && (s.opacity - o.opacity).abs() <= 1e-4
            && s.line_end == o.line_end
            && match (s.cloud, o.cloud) {
                (None, None) => true,
                (Some(a), Some(b)) => near(a, b),
                _ => false,
            };
        // A foreign FreeText that was made editable has none of our keys yet: its first
        // change of any kind that redraws it writes them all.
        let adopted = base.model.free_text.is_none();
        if !same || (adopted && (text_changed || geometry_changed)) {
            d.keys.extend(FREE_TEXT_KEYS);
            d.appearance = true;
            // Rich text of another application carries its own size, colour and alignment:
            // left as it is it would contradict the keys just written, and a viewer that
            // rebuilds the appearance from it would bring the old look back. It is written
            // again as plain paragraphs that take their style from /DS. (Rich text of ours
            // comes out as the file has it and is not mentioned by the save.)
            if base.dict.has(b"RC") {
                d.keys.insert("RC");
            }
        }
        if text_changed {
            // The rich text says what the old string said: it follows the new one.
            d.keys.insert("RC");
            d.appearance = true;
        }
    } else if text_changed && base.dict.has(b"RC") {
        // Any other markup (a note, a reply, a shape's comment): viewers show the pop-up text
        // from /RC when it is there, so a stale one would hide the new /Contents.
        d.keys.insert("RC");
    }
    let mut keep_image = false;
    if let (true, AnnotKind::Stamp, Some(stamp)) = (modelled, &current.kind, &current.stamp) {
        let same_stamp = Some(stamp) == old.stamp.as_ref();
        let same_opacity = (current.style.opacity - old.style.opacity).abs() <= 1e-4;
        if !same_opacity {
            d.keys.insert("CA");
            d.appearance = true;
        }
        if !same_stamp {
            d.keys.insert("Rotate");
            d.keys.insert(PRIVATE_KEY);
            d.appearance = true;
        }
        // A picture that is only moved or resized keeps its appearance stream: a viewer fits
        // the stream's /BBox to the new /Rect, so the pixels are not stored a second time.
        keep_image = same_stamp
            && same_opacity
            && base.has_appearance
            && matches!(stamp.content, StampContent::Image(_));
    }
    if current.kind == AnnotKind::Text
        && current.meta.reply_to.is_some() != old.meta.reply_to.is_some()
    {
        // A note that becomes a reply (or stops being one) loses (or gains) its icon.
        d.appearance = true;
    }

    // A measurement whose scale, kind or presentation changed is written again as a whole:
    // its dimension keys, its private part, its value and its appearance.
    let measure_changed = match (current.measurement(), old.measurement()) {
        (None, None) => false,
        (Some(now), Some(then)) => !now.same_as(then),
        _ => current.measure.is_some() || old.measure.is_some(),
    };
    // A measurement the file holds in another form than this version writes (a count as one
    // Ink annotation, a radius or diameter as a Line, an angle or volume of an earlier version
    // without Revu's intent) is written in the present form on the next save.
    let outdated = current.measurement().is_some_and(|m| {
        let file_name = |key: &[u8]| base.dict.get(key).ok().and_then(name_of);
        file_name(b"Subtype").as_deref() != Some(m.kind.file_subtype().pdf_name())
            || file_name(b"IT").as_deref() != m.kind.intent()
    });
    if measure_changed || outdated {
        d.keys.extend(MEASURE_KEYS);
        d.keys.insert(PRIVATE_KEY);
        d.keys.insert("Contents");
        d.appearance = true;
    }

    let (m, o) = (&current.meta, &old.meta);
    let mut meta = |key: &'static str, same: bool| {
        if !same {
            d.keys.insert(key);
        }
    };
    meta("T", m.author == o.author);
    meta("Subj", m.subject == o.subject);
    meta("Contents", m.contents == o.contents);
    meta("CreationDate", m.created == o.created);
    meta("M", m.modified == o.modified);
    meta("F", m.flags == o.flags);
    meta("OC", m.layer == o.layer);
    meta("IRT", m.reply_to == o.reply_to);
    meta("RT", m.reply_type == o.reply_type);
    meta("State", m.state == o.state);
    meta("StateModel", m.state_model == o.state_model);
    meta("Name", current.icon == old.icon);
    meta("Open", current.open == old.open);
    meta("Popup", current.popup == old.popup);
    meta("Parent", current.parent == old.parent);
    meta(PRIVATE_KEY, current.app_data == old.app_data);
    meta(COLUMN_DATA_KEY, current.column_data == old.column_data);
    meta("NM", current.id == old.id);
    // A target this crate cannot write (`Other`) is never written over what the file has;
    // every other change, clearing the target included, is.
    if current.link != old.link && !matches!(current.link, Some(LinkTarget::Other)) {
        d.keys.insert("A");
        d.keys.insert("Dest");
    }

    // The appearance is only ours to regenerate for a kind we draw.
    d.appearance &= has_generated_appearance(current)
        || (modelled && base.has_appearance && current.kind != AnnotKind::Popup);
    // An annotation that has an appearance in the file although its keys describe nothing we
    // would paint (an empty /C, a zero width) keeps that appearance for as long as the model
    // still paints nothing: a blank stream of ours would make it vanish in every viewer.
    if d.appearance
        && base.has_appearance
        && current.kind != AnnotKind::Popup
        && current.kind == old.kind
        && current.draw_list().is_empty()
        && old.draw_list().is_empty()
    {
        d.appearance = false;
        d.keep_appearance = true;
        d.keys.insert("Rect");
    }
    if d.appearance && keep_image {
        d.appearance = false;
        d.keep_appearance = true;
        d.keys.insert("Rect");
    }
    if d.appearance && current.measurement().is_some() {
        // The appearance draws the value, so the keys that say it are written with it (each
        // one only where it comes out differently from the file).
        d.keys.extend(MEASURE_KEYS);
        // The subtype and the geometry keys of the form written now; a key of another form
        // (an earlier version's /L or /InkList) comes out absent and is removed.
        d.keys.extend(["Subtype", "L", "Vertices", "InkList", "RD"]);
        d.keys.insert(PRIVATE_KEY);
        d.keys.insert("Contents");
    }
    if d.appearance {
        d.keys.insert("Rect");
        d.keys.insert("AP");
        if matches!(
            current.kind,
            AnnotKind::Square | AnnotKind::Circle | AnnotKind::FreeText
        ) {
            d.keys.insert("RD");
        }
    }
    // Every annotation this crate writes carries its id as /NM. One that came without a
    // name, or with a name another annotation also has, gets it on its first change.
    let named = matches!(
        base.dict.get(b"NM"),
        Ok(Object::String(bytes, _)) if decode_text(bytes) == current.id.as_str()
    );
    if !d.keys.is_empty() && !named {
        d.keys.insert("NM");
    }
    d
}

/// Writes an emitted appearance as a form XObject, with the resources it names, and returns
/// the stream's id. `group` makes the form a transparency group; the top-level appearance of an
/// annotation must not be one, or a Multiply blend inside it would stop acting on the page.
pub fn write_appearance(
    edits: &mut EditSet,
    appearance: &Appearance,
    group: Option<TransparencyGroup>,
) -> ObjectId {
    let mut resources = Dictionary::new();
    if !appearance.resources.ext_gstates.is_empty() {
        let mut states = Dictionary::new();
        for g in &appearance.resources.ext_gstates {
            let mut gs = Dictionary::new();
            gs.set("Type", name("ExtGState"));
            gs.set("CA", num(g.stroke_alpha));
            gs.set("ca", num(g.fill_alpha));
            gs.set("BM", name(g.blend.pdf_name()));
            states.set(g.name.as_bytes().to_vec(), Object::Dictionary(gs));
        }
        resources.set("ExtGState", Object::Dictionary(states));
    }
    if !appearance.resources.fonts.is_empty() {
        let mut fonts = Dictionary::new();
        for f in &appearance.resources.fonts {
            let mut font = Dictionary::new();
            font.set("Type", name("Font"));
            font.set("Subtype", name("Type1"));
            font.set("BaseFont", name(f.base_font));
            font.set("Encoding", name(f.encoding));
            let id = edits.add_object(Object::Dictionary(font));
            fonts.set(f.name.as_bytes().to_vec(), Object::Reference(id));
        }
        resources.set("Font", Object::Dictionary(fonts));
    }
    if !appearance.resources.xobjects.is_empty() {
        let mut xobjects = Dictionary::new();
        for x in &appearance.resources.xobjects {
            let id = match &x.object {
                XObject::Form(form) => write_appearance(edits, &form.form, form.group),
                XObject::Image(image) => {
                    let mut dict = Dictionary::new();
                    dict.set("Type", name("XObject"));
                    dict.set("Subtype", name("Image"));
                    dict.set("Width", Object::Integer(i64::from(image.width)));
                    dict.set("Height", Object::Integer(i64::from(image.height)));
                    dict.set("ColorSpace", name("DeviceRGB"));
                    dict.set("BitsPerComponent", Object::Integer(8));
                    // Samples are deflated when that makes them smaller.
                    let packed = |dict: Dictionary, data: &[u8]| {
                        let mut stream = Stream::new(dict, data.to_vec());
                        let _ = stream.compress();
                        Object::Stream(stream)
                    };
                    if let Some(alpha) = &image.alpha {
                        let mut mask = Dictionary::new();
                        mask.set("Type", name("XObject"));
                        mask.set("Subtype", name("Image"));
                        mask.set("Width", Object::Integer(i64::from(image.width)));
                        mask.set("Height", Object::Integer(i64::from(image.height)));
                        mask.set("ColorSpace", name("DeviceGray"));
                        mask.set("BitsPerComponent", Object::Integer(8));
                        let mask_id = edits.add_object(packed(mask, alpha));
                        dict.set("SMask", Object::Reference(mask_id));
                    }
                    edits.add_object(packed(dict, &image.rgb))
                }
            };
            xobjects.set(x.name.as_bytes().to_vec(), Object::Reference(id));
        }
        resources.set("XObject", Object::Dictionary(xobjects));
    }

    let b = appearance.bbox;
    let mut dict = Dictionary::new();
    dict.set("Type", name("XObject"));
    dict.set("Subtype", name("Form"));
    dict.set("FormType", Object::Integer(1));
    dict.set("BBox", nums(&[b.x0, b.y0, b.x1, b.y1]));
    if appearance.matrix != pl_geom::Affine::IDENTITY {
        dict.set("Matrix", nums(&appearance.matrix.as_coeffs()));
    }
    if let Some(g) = group {
        let mut group = Dictionary::new();
        group.set("Type", name("Group"));
        group.set("S", name("Transparency"));
        group.set("I", Object::Boolean(g.isolated));
        group.set("K", Object::Boolean(g.knockout));
        dict.set("Group", Object::Dictionary(group));
    }
    dict.set("Resources", Object::Dictionary(resources));
    edits.add_stream(dict, appearance.content.clone())
}

/// What the keys of one annotation are computed from.
struct Context<'a> {
    doc: &'a PdfDocument,
    objects: &'a HashMap<AnnotId, ObjectId>,
    placement: &'a Placement,
    appearance: Option<ObjectId>,
    base: Option<&'a Baseline>,
    /// For a count: which of its markers' annotations is being written.
    item: Option<CountItem<'a>>,
    /// The number of markers of each count group of the set ([`Annotation::count_group`]):
    /// `/NumCounts` and `/Contents` of every annotation of a group give its total.
    totals: &'a HashMap<AnnotId, usize>,
}

/// One marker's annotation of a count (see [`MeasureKind::file_subtype`]).
#[derive(Debug, Clone)]
struct CountItem<'a> {
    /// 0 for the count's own annotation, then 1, 2, ... for the further ones.
    index: usize,
    /// The count's own annotation, which the further ones are grouped under.
    head: ObjectId,
    /// `/NM` of a further one.
    name: AnnotId,
    /// The file's dictionary of the count's own annotation, when it has one: a further
    /// annotation takes from it the keys the model does not hold (Revu's `/Version`, `/DS`,
    /// `/Label`, ...) and its `/BBMeasure` object while that still reads as the count's scale.
    head_file: Option<&'a Dictionary>,
}

/// The keys a changed count can have patched on its own annotation alone, the rest of it (and
/// its other markers' annotations) left as the file has them.
const COUNT_PATCHED_KEYS: [&str; 6] = [COLUMN_DATA_KEY, "M", "T", "Subj", "F", "OC"];

/// Keys of another application's count (Bluebeam Revu's) that the model does not hold. Each
/// further annotation written for the count carries them as its first annotation has them,
/// as every item of a Revu count does.
const COUNT_SHARED_KEYS: [&str; 9] = [
    "Version",
    "DS",
    "Label",
    "DepthUnit",
    "FillOpacity",
    "SlopeType",
    "PitchRun",
    "AlignOnSegment",
    "RiseDrop",
];

/// The file's dictionary a count annotation is described by: its own, or for a further one
/// its count's first annotation's.
fn count_file_dict<'a>(ctx: &Context<'a>) -> Option<&'a Dictionary> {
    ctx.base
        .map(|b| &b.dict)
        .or_else(|| ctx.item.as_ref().and_then(|i| i.head_file))
}

/// `/Contents` of a measurement: its value as Revu writes it (no `R` or `Ø`). A count's is
/// the total of its group, after its `/Label` and a CR when the file has a label, as Revu
/// writes it (`Check Mark\r15`).
fn measure_contents(a: &Annotation, ctx: &Context<'_>) -> Option<String> {
    let m = a.measurement()?;
    if m.kind != MeasureKind::Count {
        return a.measure_value_text();
    }
    let total = count_total(a, ctx);
    let label = count_file_dict(ctx)
        .and_then(|d| lookup(ctx.doc, d, b"Label"))
        .and_then(crate::objects::text_of)
        .filter(|l| !l.is_empty());
    Some(match label {
        Some(label) => format!("{label}\r{total}"),
        None => total.to_string(),
    })
}

/// The number of markers of a count's group.
fn count_total(a: &Annotation, ctx: &Context<'_>) -> usize {
    a.count_group()
        .and_then(|g| ctx.totals.get(&g).copied())
        .unwrap_or_else(|| a.count_points().len())
}

/// True for a radius or a diameter written in Revu's form (a Polygon or a Circle): its line
/// endings and the fill of its arrowheads are the model's, not the file shape's (`/IC` on a
/// Circle fills the circle), so they are kept in the private part instead.
fn revu_round(a: &Annotation) -> bool {
    a.measurement().is_some_and(|m| {
        matches!(m.kind, MeasureKind::Radius | MeasureKind::Diameter)
            && m.kind.file_subtype() != m.kind.carrier()
    })
}

fn color_object(c: Rgb) -> Object {
    let c = c.clamped();
    nums(&[f64::from(c.r), f64::from(c.g), f64::from(c.b)])
}

fn points_object(points: &[Point]) -> Object {
    Object::Array(points.iter().flat_map(|p| [num(p.x), num(p.y)]).collect())
}

/// The value of `key` for `a`; `None` means the key is absent.
fn key_object(a: &Annotation, key: &str, ctx: &Context<'_>) -> Result<Option<Object>, AnnotError> {
    let styled = |k: &str| a.is_modelled() && style_keys(&a.kind).contains(&k);
    let link = |id: &Option<AnnotId>| {
        id.as_ref()
            .and_then(|id| ctx.objects.get(id))
            .map(|o| Object::Reference(*o))
    };
    let text = |s: &Option<String>| s.as_deref().map(encode_text);
    let s = &a.style;
    if let Some(m) = a.measurement() {
        let count = m.kind == MeasureKind::Count;
        match key {
            // The total of the count's group, on every annotation of it (Revu's convention).
            "NumCounts" if count => {
                return Ok(Some(Object::Integer(count_total(a, ctx) as i64)));
            }
            // Revu's Polygon radius and Circle diameter have no /CP, /IC or /LE: on those
            // shapes /IC fills the circle. The model's are in the private part.
            "CP" | "IC" | "LE" if revu_round(a) => return Ok(None),
            // A filled marker (Revu's /Circle style with /IC); the others are outlines.
            "IC" if count => {
                let dot = (m.symbol == CountSymbol::Dot).then_some(s.stroke).flatten();
                return Ok(s.fill.or(dot).map(color_object));
            }
            // A further annotation of a count shares its first one's measure dictionary while
            // that still reads as the count's scale, as Revu's items share one object.
            "BBMeasure" if count && ctx.item.as_ref().is_some_and(|i| i.index > 0) => {
                if let Some(head) = ctx.item.as_ref().and_then(|i| i.head_file) {
                    if head.has(b"BBMeasure")
                        && crate::measure_io::file_spec(ctx.doc, head, b"BBMeasure").as_ref()
                            == m.spec.as_ref()
                    {
                        return Ok(head.get(b"BBMeasure").ok().cloned());
                    }
                }
            }
            "Contents" => {
                return Ok(measure_contents(a, ctx)
                    .map(|t| encode_text(&t))
                    .or_else(|| text(&a.meta.contents)));
            }
            // Rich text follows /Contents where the file (or the count's first annotation)
            // has it.
            "RC" if count_file_dict(ctx).is_some_and(|d| d.has(b"RC")) => {
                return Ok(measure_contents(a, ctx)
                    .or_else(|| a.meta.contents.clone())
                    .map(|c| encode_text(&rich_text(&c))));
            }
            _ => {}
        }
    }
    if a.measurement().is_some() && MEASURE_KEYS.contains(&key) {
        return Ok(measure_key(a, key));
    }
    let measured = a.measurement();
    let file_kind = measured.map(|m| m.kind.file_subtype());
    if let (Some(item), Some(m)) = (&ctx.item, measured) {
        let member = item.index > 0;
        match key {
            "Vertices" => {
                return Ok(a.count_points().get(item.index).map(|p| {
                    points_object(&crate::measure_draw::symbol_outline(
                        m.symbol,
                        *p,
                        m.symbol_size,
                        m.rotation,
                    ))
                }));
            }
            "NM" if member => return Ok(Some(encode_text(item.name.as_str()))),
            // The further markers are grouped under the first, as Revu groups its counts.
            "IRT" if member => return Ok(Some(Object::Reference(item.head))),
            "RT" if member => return Ok(Some(name("Group"))),
            "Popup" if member => return Ok(None),
            // The count's column values are on its first annotation, the one its row reads.
            k if k == COLUMN_DATA_KEY && member => return Ok(None),
            k if k == PRIVATE_KEY && member => {
                let mut own = Dictionary::new();
                own.set("Type", name("Measure"));
                own.set("Kind", name(m.kind.name()));
                own.set("Group", encode_text(a.id.as_str()));
                own.set("Item", Object::Integer(item.index as i64));
                if let Some(p) = a.count_points().get(item.index) {
                    own.set("At", crate::measure_io::exact_point_object(*p));
                }
                let mut d = Dictionary::new();
                d.set(MODEL_KEY, Object::Dictionary(own));
                return Ok(Some(Object::Dictionary(d)));
            }
            _ => {}
        }
    }
    Ok(match key {
        "Type" => Some(name("Annot")),
        "Subtype" => Some(match a.measurement() {
            Some(m) => name(m.kind.file_subtype().pdf_name()),
            None => name(a.kind.pdf_name()),
        }),
        "Rect" => {
            let r = ctx.placement.rect;
            Some(nums(&[r.x0, r.y0, r.x1, r.y1]))
        }
        // A diameter's Circle: /Rect less /RD is the circle (left, bottom, right, top).
        "RD" if file_kind == Some(AnnotKind::Circle) => match &a.geometry {
            Geometry::Line { a: p, b: q } => {
                let (c, r) = (p.midpoint(*q), p.distance(*q) / 2.0);
                let rect = ctx.placement.rect;
                Some(nums(&[
                    (c.x - r - rect.x0).max(0.0),
                    (c.y - r - rect.y0).max(0.0),
                    (rect.x1 - c.x - r).max(0.0),
                    (rect.y1 - c.y - r).max(0.0),
                ]))
            }
            _ => None,
        },
        "RD" if file_kind.is_some() => None,
        "RD" => ctx.placement.rect_differences.map(|d| nums(&d)),
        "AP" => ctx.appearance.map(|id| {
            let mut ap = Dictionary::new();
            ap.set("N", Object::Reference(id));
            Object::Dictionary(ap)
        }),
        "NM" => Some(encode_text(a.id.as_str())),
        "P" => {
            let page = ctx
                .doc
                .page_id(a.page)
                .map_err(|_| AnnotError::PageOutOfRange {
                    id: a.id.clone(),
                    page: a.page,
                    count: ctx.doc.page_count(),
                })?;
            Some(Object::Reference(page))
        }
        "F" => Some(Object::Integer(i64::from(a.meta.flags.0))),
        "M" => a.meta.modified.map(|d| encode_text(&d.to_pdf_string())),
        "CreationDate" => a.meta.created.map(|d| encode_text(&d.to_pdf_string())),
        "T" => text(&a.meta.author),
        "Subj" => text(&a.meta.subject),
        // A measurement's text is its value, worked out afresh from its geometry and scale.
        "Contents" => a
            .measure_text()
            .map(|t| encode_text(&t))
            .or_else(|| text(&a.meta.contents)),
        // Leaving /C out would not say "no colour" for a kind viewers paint in a default
        // colour; the empty array does.
        "C" if styled("C") && a.kind == AnnotKind::FreeText => s.fill.map(color_object),
        "C" if styled("C") => s
            .stroke
            .map(color_object)
            .or_else(|| a.kind.implied_color().map(|_| Object::Array(Vec::new()))),
        "IC" if styled("IC") => s.fill.map(color_object),
        "CA" if styled("CA") => Some(num(s.opacity.clamp(0.0, 1.0))),
        "BS" if styled("BS") => {
            // Start from the file's /BS so keys we do not model survive.
            let mut bs = ctx
                .base
                .and_then(|b| lookup(ctx.doc, &b.dict, b"BS"))
                .and_then(|o| o.as_dict().ok())
                .cloned()
                .unwrap_or_else(|| {
                    let mut d = Dictionary::new();
                    d.set("Type", name("Border"));
                    d
                });
            let borderless = a.kind == AnnotKind::FreeText && s.stroke.is_none();
            bs.set("W", num(if borderless { 0.0 } else { s.width.max(0.0) }));
            let was_dashed = bs.get(b"S").ok().and_then(name_of).as_deref() == Some("D");
            match &s.dash {
                Some(dash) => {
                    bs.set("S", name("D"));
                    bs.set("D", nums(dash));
                }
                None => {
                    if was_dashed || !bs.has(b"S") {
                        bs.set("S", name("S"));
                    }
                    bs.remove(b"D");
                }
            }
            Some(Object::Dictionary(bs))
        }
        "BE" if styled("BE") => s.cloud.map(|intensity| {
            let mut be = Dictionary::new();
            be.set("S", name("C"));
            be.set("I", num(intensity));
            Object::Dictionary(be)
        }),
        "LE" if a.kind == AnnotKind::FreeText => free_text_of(a)
            .filter(|t| !t.callout.is_empty() || s.line_end != pl_draw::LineEnding::None)
            .map(|_| name(s.line_end.pdf_name())),
        "DA" => free_text_of(a)
            .map(|t| encode_text(&default_appearance(s.stroke, t.color, t.font, t.size))),
        "DS" => {
            free_text_of(a).map(|t| encode_text(&default_style(t.color, t.font, t.size, t.align)))
        }
        // A text box of ours always has rich text; any other markup only keeps in step the
        // rich text its file came with.
        "RC" => a
            .meta
            .contents
            .as_deref()
            .filter(|_| free_text_of(a).is_some() || ctx.base.is_some_and(|b| b.dict.has(b"RC")))
            .map(|c| encode_text(&rich_text(c))),
        "Q" => free_text_of(a).map(|t| Object::Integer(t.align.quadding())),
        "IT" => free_text_of(a).map(|t| name(t.intent.pdf_name())),
        "CL" => free_text_of(a)
            .filter(|t| t.callout.len() >= 2)
            .map(|t| points_object(&t.callout)),
        "Rotate" => Some(a.rotation())
            .filter(|r| *r != 0 && a.is_modelled())
            .map(|r| Object::Integer(i64::from(r))),
        "LE" if styled("LE") => Some(Object::Array(vec![
            name(s.line_start.pdf_name()),
            name(s.line_end.pdf_name()),
        ])),
        "BM" if styled("BM") => {
            (s.blend != a.kind.default_blend()).then(|| name(s.blend.pdf_name()))
        }
        "L" if file_kind.as_ref().is_some_and(|k| *k != AnnotKind::Line) => None,
        "L" => match &a.geometry {
            Geometry::Line { a, b } => Some(nums(&[a.x, a.y, b.x, b.y])),
            _ => None,
        },
        // Revu's radius: a point on the circle, the centre, the point again (a whole circle).
        "Vertices" if file_kind == Some(AnnotKind::Polygon) && a.kind == AnnotKind::Line => {
            match &a.geometry {
                Geometry::Line { a: centre, b: edge } => Some(points_object(&[*edge, *centre, *edge])),
                _ => None,
            }
        }
        "Vertices" => match &a.geometry {
            // A perimeter is a closed PolyLine in the file: the first vertex again at the end.
            Geometry::Points(points)
                if a.measurement().map(|m| m.kind) == Some(MeasureKind::Perimeter) =>
            {
                let mut closed = points.clone();
                closed.extend(points.first().copied());
                Some(points_object(&closed))
            }
            Geometry::Points(points) => Some(points_object(points)),
            _ => None,
        },
        "InkList" if file_kind.as_ref().is_some_and(|k| *k != AnnotKind::Ink) => None,
        "InkList" => match &a.geometry {
            Geometry::Ink(strokes) => Some(Object::Array(
                strokes.iter().map(|s| points_object(s)).collect(),
            )),
            _ => None,
        },
        "QuadPoints" => match &a.geometry {
            Geometry::Quads(quads) => Some(Object::Array(
                quads
                    .iter()
                    .flat_map(|q| quad_to_file(q).map(num))
                    .collect(),
            )),
            _ => None,
        },
        "Name" => a.icon.as_deref().map(name),
        "Open" => match a.kind {
            AnnotKind::Popup | AnnotKind::Text => Some(Object::Boolean(a.open)),
            _ => a.open.then_some(Object::Boolean(true)),
        },
        "Popup" => link(&a.popup),
        "Parent" => link(&a.parent),
        "IRT" => link(&a.meta.reply_to),
        "RT" => a.meta.reply_type.as_deref().map(name),
        "State" => text(&a.meta.state),
        "StateModel" => text(&a.meta.state_model),
        "OC" => a.meta.layer.map(Object::Reference),
        "A" => match &a.link {
            Some(LinkTarget::Uri(uri)) => {
                let mut action = Dictionary::new();
                action.set("Type", name("Action"));
                action.set("S", name("URI"));
                action.set(
                    "URI",
                    Object::String(uri.as_bytes().to_vec(), StringFormat::Literal),
                );
                Some(Object::Dictionary(action))
            }
            _ => None,
        },
        "Dest" => match &a.link {
            Some(LinkTarget::Page { page, at }) => {
                let target = ctx
                    .doc
                    .page_id(*page)
                    .map_err(|_| AnnotError::PageOutOfRange {
                        id: a.id.clone(),
                        page: *page,
                        count: ctx.doc.page_count(),
                    })?;
                let mut dest = vec![Object::Reference(target)];
                match at {
                    Some(p) => dest.extend([name("XYZ"), num(p.x), num(p.y), Object::Null]),
                    None => dest.push(name("Fit")),
                }
                Some(Object::Array(dest))
            }
            _ => None,
        },
        k if k == PRIVATE_KEY => private_object(a),
        k if k == COLUMN_DATA_KEY => a.column_data.clone().map(Object::Array),
        _ => None,
    })
}

/// Every key a new annotation's dictionary can have, in the order it is written.
const ALL_KEYS: [&str; 55] = [
    "Type",
    "Subtype",
    "Rect",
    "RD",
    "NM",
    "P",
    "F",
    "M",
    "CreationDate",
    "T",
    "Subj",
    "Contents",
    "C",
    "IC",
    "CA",
    "BS",
    "BE",
    "LE",
    "BM",
    "DA",
    "DS",
    "RC",
    "Q",
    "IT",
    "CL",
    "Measure",
    "LL",
    "LLE",
    "LLO",
    "Cap",
    "CP",
    "CO",
    "MeasurementTypes",
    "BBMeasure",
    "CountStyle",
    "NumCounts",
    "Rotate",
    "L",
    "Vertices",
    "InkList",
    "QuadPoints",
    "Name",
    "Open",
    "Popup",
    "Parent",
    "IRT",
    "RT",
    "State",
    "StateModel",
    "OC",
    "A",
    "Dest",
    "AP",
    PRIVATE_KEY,
    COLUMN_DATA_KEY,
];

/// Generates the appearance stream of `a`, if it is a kind that has one of ours.
fn generate_appearance(
    edits: &mut EditSet,
    a: &Annotation,
    placement: &Placement,
) -> Result<Option<ObjectId>, AnnotError> {
    if !has_generated_appearance(a) {
        return Ok(None);
    }
    // Text that cannot be drawn is refused here, by name; it is never left out of the stream.
    a.validate().map_err(|source| AnnotError::Draw {
        id: a.id.clone(),
        source,
    })?;
    let appearance =
        pl_draw::emit_with_bbox(&placement.list, placement.rect).map_err(|source| {
            AnnotError::Draw {
                id: a.id.clone(),
                source,
            }
        })?;
    Ok(Some(write_appearance(edits, &appearance, None)))
}

fn translated(object: &Object, by: Vec2) -> Option<Object> {
    let array = object.as_array().ok()?;
    if array.iter().all(|o| matches!(o, Object::Array(_))) && !array.is_empty() {
        return array
            .iter()
            .map(|o| translated(o, by))
            .collect::<Option<Vec<_>>>()
            .map(Object::Array);
    }
    if array.len() % 2 != 0 {
        return None;
    }
    let values: Option<Vec<f64>> = array.iter().map(pl_pdf::number).collect();
    Some(Object::Array(
        values?
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|c| [num(c[0] + by.x), num(c[1] + by.y)])
            .collect(),
    ))
}

/// `/Rect` for an annotation that keeps the file's appearance: the file's own rectangle moved
/// by as much as the geometry moved, so the appearance is neither scaled nor shifted inside
/// it. When the geometry changed size, the rectangle a new appearance would get.
fn carried_rect(doc: &PdfDocument, base: &Baseline, a: &Annotation, fallback: Rect) -> Rect {
    let old = lookup(doc, &base.dict, b"Rect")
        .and_then(|o| crate::objects::number_array(doc, o))
        .filter(|v| v.len() == 4)
        .map(|v| Rect::new(v[0], v[1], v[2], v[3]).abs());
    let (was, now) = (base.model.geometry.bounds(), a.geometry.bounds());
    match old {
        Some(old) if near(was.width(), now.width()) && near(was.height(), now.height()) => {
            let by = Vec2::new(now.x0 - was.x0, now.y0 - was.y0);
            Rect::new(old.x0 + by.x, old.y0 + by.y, old.x1 + by.x, old.y1 + by.y)
        }
        _ => fallback,
    }
}

/// Every key of a new annotation's dictionary, and their names, sorted.
fn full_dict(a: &Annotation, ctx: &Context<'_>) -> Result<(Dictionary, Vec<String>), AnnotError> {
    let mut dict = Dictionary::new();
    let mut keys = Vec::new();
    for key in ALL_KEYS {
        if let Some(value) = key_object(a, key, ctx)? {
            dict.set(key, value);
            keys.push(key.to_string());
        }
    }
    keys.sort();
    Ok((dict, keys))
}

/// True when the measurement in a file's dictionary is this crate's (its private part says
/// so), not another application's.
fn ours_in_file(doc: &PdfDocument, dict: &Dictionary) -> bool {
    lookup(doc, dict, PRIVATE_KEY.as_bytes())
        .and_then(|o| o.as_dict().ok())
        .and_then(|p| lookup(doc, p, MODEL_KEY.as_bytes()))
        .and_then(|o| o.as_dict().ok())
        .and_then(|m| lookup(doc, m, b"Type"))
        .and_then(name_of)
        .as_deref()
        == Some("Measure")
}

/// True when the file's value of `key` stands although the model would write it otherwise:
/// a measure dictionary that reads as the measurement's own scale (another application's
/// keys in it are kept), and Revu's keys on a measurement of another application's.
fn file_value_stands(a: &Annotation, key: &str, ctx: &Context<'_>) -> bool {
    let (Some(base), Some(m)) = (ctx.base, a.measurement()) else {
        return false;
    };
    if !base.dict.has(key.as_bytes()) {
        return false;
    }
    match key {
        "Measure" | "BBMeasure" => {
            crate::measure_io::file_spec(ctx.doc, &base.dict, key.as_bytes()).as_ref()
                == m.spec.as_ref()
        }
        k if crate::measure_io::REVU_KEYS.contains(&k) => !ours_in_file(ctx.doc, &base.dict),
        _ => false,
    }
}

/// The patch that gives the file's dictionary the model's value of each of `keys`, and the
/// keys it names. A key whose value comes out as the file already has it (a `/Rect` that did
/// not move, say) is left alone.
fn patch_keys<'k>(
    a: &Annotation,
    keys: impl Iterator<Item = &'k str>,
    ctx: &Context<'_>,
) -> Result<(DictPatch, BTreeSet<String>), AnnotError> {
    let base = ctx.base.expect("a patch is of a dictionary in the file");
    let mut patch = DictPatch::new();
    let mut names: BTreeSet<String> = BTreeSet::new();
    for key in keys {
        if file_value_stands(a, key, ctx) {
            continue;
        }
        match key_object(a, key, ctx)? {
            Some(value) => {
                if base.dict.get(key.as_bytes()).ok() != Some(&value) {
                    patch.set(key, value);
                    names.insert(key.to_string());
                }
            }
            None => {
                if base.dict.has(key.as_bytes()) {
                    patch.remove(key);
                    names.insert(key.to_string());
                }
            }
        }
    }
    Ok((patch, names))
}

/// The keys of a count's first annotation that are written whenever the count is: its form,
/// its marker and what it shows. Other keys the file has (another application's `/DS`, say)
/// are left alone unless the model changed them.
const COUNT_ITEM_KEYS: [&str; 11] = [
    "Subtype",
    "Rect",
    "AP",
    "Vertices",
    "InkList",
    "L",
    "RD",
    "IC",
    "Contents",
    "NM",
    PRIVATE_KEY,
];

/// True when the `/Contents` the save would write differs from the file's.
fn contents_change(a: &Annotation, ctx: &Context<'_>, base: &Baseline) -> bool {
    matches!(key_object(a, "Contents", ctx), Ok(v) if base.dict.get(b"Contents").ok() != v.as_ref())
}

/// The keys of an annotation of a count group that the group's total is written in.
const COUNT_TOTAL_KEYS: [&str; 2] = ["NumCounts", "Contents"];

/// Writes a count as Revu does: one small Polygon (`/IT /PolygonCount`) per marker, drawing
/// the marker's symbol. The first is the count's own annotation; the others are new objects
/// grouped under it (`/IRT`, `/RT /Group`) and named in the private part of each, so that a
/// load reads them back as this one count. A count that is written again replaces all its
/// further annotations. Returns the change, and whether the count is new.
fn write_count(
    doc: &PdfDocument,
    edits: &mut EditSet,
    a: &Annotation,
    d: Option<&Diff>,
    new_id: Option<ObjectId>,
    objects: &HashMap<AnnotId, ObjectId>,
    totals: &HashMap<AnnotId, usize>,
) -> Result<(Change, bool), AnnotError> {
    let m = a.measurement().expect("a count");
    let (head, base) = match (&a.baseline, new_id) {
        (Some(base), _) => {
            let object = base
                .object
                .ok_or_else(|| AnnotError::NotEditable(a.id.clone()))?;
            if !matches!(doc.object(object), Some(Object::Dictionary(d)) if *d == base.dict) {
                return Err(AnnotError::Stale(a.id.clone()));
            }
            (object, Some(base))
        }
        (None, Some(object)) => (object, None),
        (None, None) => unreachable!("a new annotation always has a reserved id"),
    };
    let points = a.count_points();
    let half = a.style.width.max(0.0) / 2.0;
    let mut change = Change {
        id: a.id.clone(),
        page: a.page,
        object: head,
        keys: Vec::new(),
        appearance: None,
        members: Vec::new(),
    };
    for index in 0..points.len().max(1) {
        let list = crate::measure_draw::count_item_list(a, m, index);
        let shape = points.get(index).map_or_else(
            || a.geometry.bounds(),
            |p| {
                crate::measure_io::bounds_of(&crate::measure_draw::symbol_outline(
                    m.symbol,
                    *p,
                    m.symbol_size,
                    m.rotation,
                ))
            },
        );
        let shape = shape.inflate(half, half);
        let placement = Placement {
            rect: crate::appearance::round_out(list.bounds().map_or(shape, |b| b.union(shape))),
            rect_differences: None,
            list,
        };
        let appearance = generate_appearance(edits, a, &placement)?;
        let object = if index == 0 { head } else { edits.reserve_id() };
        let head_file = base.map(|b| &b.dict);
        let ctx = Context {
            doc,
            objects,
            placement: &placement,
            appearance,
            base: if index == 0 { base } else { None },
            item: Some(CountItem {
                index,
                head,
                name: AnnotId(format!("{}.{index}", a.id)),
                head_file,
            }),
            totals,
        };
        if index > 0 {
            let (mut dict, _) = full_dict(a, &ctx)?;
            // What every item of another application's count carries and the model does not.
            for key in COUNT_SHARED_KEYS {
                if !dict.has(key.as_bytes()) {
                    if let Some(value) = head_file.and_then(|h| h.get(key.as_bytes()).ok()) {
                        dict.set(key, value.clone());
                    }
                }
            }
            edits.set_object(object, Object::Dictionary(dict));
            change.members.push(object);
            continue;
        }
        change.appearance = appearance;
        match base {
            None => {
                let (dict, keys) = full_dict(a, &ctx)?;
                edits.set_object(object, Object::Dictionary(dict));
                change.keys = keys;
            }
            Some(b) => {
                // What the diff names, and every key that says where the first marker is
                // and what it shows (all of them follow from the whole count).
                let mut names: BTreeSet<&str> = d
                    .iter()
                    .flat_map(|d| d.keys.iter().copied())
                    .chain(COUNT_ITEM_KEYS)
                    .chain(MEASURE_KEYS)
                    .collect();
                // Rich text only follows a /Contents that changes: another application's
                // own rich text stays while it still says the same.
                if contents_change(a, &ctx, b) {
                    names.insert("RC");
                } else {
                    names.remove("RC");
                }
                let (patch, keys) = patch_keys(a, names.into_iter(), &ctx)?;
                if !patch.is_empty() {
                    edits.modify_annotation(object, patch);
                }
                change.keys = keys.into_iter().collect();
            }
        }
    }
    for member in base.iter().flat_map(|b| &b.members) {
        if let Some(object) = member.object {
            edits.delete_annotation(member.page, object);
        }
    }
    Ok((change, base.is_none()))
}

/// The patch of an unchanged annotation of a count group whose total is not the one its
/// `/NumCounts` says (another annotation of the group gained or lost a marker): the new total
/// in `/NumCounts` and `/Contents` (and `/RC` when it has one). `None` when there is nothing
/// to write, and for an annotation with no `/NumCounts` of its own.
fn count_total_patch(
    doc: &PdfDocument,
    edits: &mut EditSet,
    a: &Annotation,
    objects: &HashMap<AnnotId, ObjectId>,
    totals: &HashMap<AnnotId, usize>,
) -> Result<Option<Change>, AnnotError> {
    let (Some(base), Some(group)) = (&a.baseline, a.count_group()) else {
        return Ok(None);
    };
    let Some(object) = base.object else {
        return Ok(None);
    };
    let filed = lookup(doc, &base.dict, b"NumCounts").and_then(|o| o.as_i64().ok());
    let total = totals.get(&group).copied().unwrap_or(0) as i64;
    if filed.is_none_or(|n| n == total) {
        return Ok(None);
    }
    if !matches!(doc.object(object), Some(Object::Dictionary(d)) if *d == base.dict) {
        return Err(AnnotError::Stale(a.id.clone()));
    }
    let placement = a.placement();
    let ctx = Context {
        doc,
        objects,
        placement: &placement,
        appearance: None,
        base: Some(base),
        item: None,
        totals,
    };
    let mut names: Vec<&str> = COUNT_TOTAL_KEYS.to_vec();
    if contents_change(a, &ctx, base) {
        names.push("RC");
    }
    let (patch, keys) = patch_keys(a, names.into_iter(), &ctx)?;
    if patch.is_empty() {
        return Ok(None);
    }
    edits.modify_annotation(object, patch);
    Ok(Some(Change {
        id: a.id.clone(),
        page: a.page,
        object,
        keys: keys.into_iter().collect(),
        appearance: None,
        members: Vec::new(),
    }))
}

/// The coordinate keys an unmodelled annotation may carry besides `/Rect`. When such an
/// annotation is moved without being resized they are moved with it.
const COORDINATE_KEYS: [&str; 5] = ["L", "Vertices", "CL", "QuadPoints", "InkList"];

impl AnnotSet {
    /// Works out what a save has to write to make `doc` hold this set, as a pl-pdf edit set.
    ///
    /// `doc` must be the document state the set was loaded from (or last rebased on).
    /// Annotations whose fields still match the file are left out of the edit set entirely.
    /// The set itself is not changed: after the save succeeds, load a new set from the saved
    /// document (ids are stable) or call [`AnnotSet::rebase`].
    pub fn plan_save(&self, doc: &PdfDocument) -> Result<SavePlan, AnnotError> {
        let page_count = doc.page_count();
        let mut edits = doc.edit();

        // Every annotation's object id, new ones included, so they can refer to each other.
        let mut objects: HashMap<AnnotId, ObjectId> = HashMap::new();
        let mut seen: HashSet<&AnnotId> = HashSet::new();
        let mut diffs: Vec<Option<Diff>> = Vec::with_capacity(self.len());
        for a in self.iter() {
            if !seen.insert(&a.id) {
                return Err(AnnotError::DuplicateId(a.id.clone()));
            }
            if a.page >= page_count {
                return Err(AnnotError::PageOutOfRange {
                    id: a.id.clone(),
                    page: a.page,
                    count: page_count,
                });
            }
            match &a.baseline {
                Some(base) => {
                    if let Some(object) = base.object {
                        objects.insert(a.id.clone(), object);
                    }
                    diffs.push(Some(diff(a, base)));
                }
                None => diffs.push(None),
            }
        }
        // New annotations get their object ids up front.
        let mut new_ids: Vec<Option<ObjectId>> = Vec::with_capacity(self.len());
        for a in self.iter() {
            new_ids.push(if a.baseline.is_none() {
                let id = edits.reserve_id();
                objects.insert(a.id.clone(), id);
                Some(id)
            } else {
                None
            });
        }

        // The markers of each count group: Revu writes one annotation per marker, grouped
        // under the first, and the group's total on each.
        let mut totals: HashMap<AnnotId, usize> = HashMap::new();
        for a in self.iter() {
            if let Some(group) = a.count_group() {
                *totals.entry(group).or_default() += a.count_points().len();
            }
        }

        let mut added = Vec::new();
        let mut modified = Vec::new();
        // The further annotations of each count, attached after it below.
        let mut count_members: HashMap<AnnotId, Vec<ObjectId>> = HashMap::new();
        for ((a, d), new_id) in self.iter().zip(&diffs).zip(&new_ids) {
            if d.as_ref().is_some_and(|d| d.keys.is_empty()) {
                // An unchanged annotation of a count group whose total changed (a marker was
                // added to or taken off another of its annotations) takes the new total.
                if let Some(change) = count_total_patch(doc, &mut edits, a, &objects, &totals)? {
                    modified.push(change);
                }
                continue;
            }
            if !a.geometry.is_finite() {
                return Err(AnnotError::NotFinite(a.id.clone()));
            }
            // A count whose change is only to keys that say nothing of its markers (its
            // column values, its author, its date) is patched like any annotation: its markers'
            // annotations and appearances stay as the file has them.
            let markers_kept = d.as_ref().is_some_and(|d| {
                !d.appearance && d.keys.iter().all(|k| COUNT_PATCHED_KEYS.contains(k))
            });
            if !markers_kept
                && a
                    .measurement()
                    .is_some_and(|m| m.kind == MeasureKind::Count)
            {
                let (change, is_new) =
                    write_count(doc, &mut edits, a, d.as_ref(), *new_id, &objects, &totals)?;
                count_members.insert(a.id.clone(), change.members.clone());
                if is_new {
                    added.push(change);
                } else if !change.keys.is_empty() || !change.members.is_empty() {
                    modified.push(change);
                }
                continue;
            }
            let mut placement = a.placement();
            if let (Some(base), Some(d)) = (&a.baseline, d) {
                if d.keep_appearance {
                    placement.rect = carried_rect(doc, base, a, placement.rect);
                }
            }
            match (&a.baseline, d, new_id) {
                (None, _, Some(object)) => {
                    if !a.is_modelled() {
                        return Err(AnnotError::Unmodelled(a.id.clone()));
                    }
                    let appearance = generate_appearance(&mut edits, a, &placement)?;
                    let ctx = Context {
                        doc,
                        objects: &objects,
                        placement: &placement,
                        appearance,
                        base: None,
                        item: None,
                        totals: &totals,
                    };
                    let (dict, keys) = full_dict(a, &ctx)?;
                    edits.set_object(*object, Object::Dictionary(dict));
                    added.push(Change {
                        id: a.id.clone(),
                        page: a.page,
                        object: *object,
                        keys,
                        appearance,
                        members: Vec::new(),
                    });
                }
                (Some(base), Some(d), _) => {
                    let object = base
                        .object
                        .ok_or_else(|| AnnotError::NotEditable(a.id.clone()))?;
                    if !matches!(doc.object(object), Some(Object::Dictionary(d)) if *d == base.dict)
                    {
                        return Err(AnnotError::Stale(a.id.clone()));
                    }
                    let appearance = if d.appearance {
                        generate_appearance(&mut edits, a, &placement)?
                    } else {
                        None
                    };
                    let ctx = Context {
                        doc,
                        objects: &objects,
                        placement: &placement,
                        appearance,
                        base: Some(base),
                        item: None,
                        totals: &totals,
                    };
                    let (mut patch, mut keys) = patch_keys(a, d.keys.iter().copied(), &ctx)?;
                    // An annotation we cannot redraw keeps its own appearance; when it is only
                    // moved, its other coordinates move with it.
                    if !d.appearance && d.keys.contains("Rect") {
                        if let (Geometry::Rect(new), Geometry::Rect(old)) =
                            (&a.geometry, &base.model.geometry)
                        {
                            let same_size =
                                near(new.width(), old.width()) && near(new.height(), old.height());
                            if same_size && !a.is_modelled() {
                                let by = new.origin() - old.origin();
                                for key in COORDINATE_KEYS {
                                    let moved = lookup(doc, &base.dict, key.as_bytes())
                                        .and_then(|o| translated(o, by));
                                    if let Some(value) = moved {
                                        patch.set(key, value);
                                        keys.insert(key.to_string());
                                    }
                                }
                            }
                        }
                    }
                    if patch.is_empty() {
                        continue;
                    }
                    // A count patched alone: the further annotations it is written as carry
                    // the same column values, author, date, subject, flags and layer as its
                    // first one, so each takes the same patch (its marker is left as it is).
                    if markers_kept {
                        for member in &base.members {
                            if let Some(m) = member.object {
                                if matches!(doc.object(m), Some(Object::Dictionary(_))) {
                                    edits.modify_annotation(m, patch.clone());
                                }
                            }
                        }
                    }
                    edits.modify_annotation(object, patch);
                    modified.push(Change {
                        id: a.id.clone(),
                        page: a.page,
                        object,
                        keys: keys.into_iter().collect(),
                        appearance,
                        members: Vec::new(),
                    });
                }
                _ => unreachable!("a new annotation always has a reserved id"),
            }
        }

        // Deletions: annotations of the file that are no longer in the set.
        // One object can be listed more than once (on two pages, or twice on one, as
        // page-duplicating tools leave it); each listing is its own entry of the set, so a
        // deletion is of one /Annots slot, not of the object.
        let mut deleted = Vec::new();
        let mut gone: HashSet<(usize, usize)> = HashSet::new();
        for r in self.removed() {
            let Some(base) = &r.baseline else { continue };
            let object = base
                .object
                .ok_or_else(|| AnnotError::NotEditable(r.id.clone()))?;
            if !gone.insert((base.page, base.slot)) {
                continue;
            }
            deleted.push(Deletion {
                id: r.id.clone(),
                page: base.page,
                object,
            });
        }

        // /Annots arrays. A page keeps its array order unless the stacking order changed.
        let mut reordered_pages = Vec::new();
        let pages: BTreeSet<usize> = self
            .iter()
            .map(|a| a.page)
            .chain(
                self.iter()
                    .filter_map(|a| a.baseline.as_ref().map(|b| b.page)),
            )
            .collect();
        for r in &deleted {
            edits.delete_annotation(r.page, r.object);
        }
        // A deleted count goes with all the annotations it is written as.
        for r in self.removed() {
            for m in r.baseline.iter().flat_map(|b| &b.members) {
                if let Some(object) = m.object {
                    edits.delete_annotation(m.page, object);
                }
            }
        }
        for page in pages {
            // Annotations that were on this page in the file and still are, in file order.
            let mut staying: Vec<(usize, ObjectId)> = self
                .iter()
                .filter(|a| a.page == page)
                .filter_map(|a| a.baseline.as_ref())
                .filter(|b| b.page == page)
                .filter_map(|b| b.object.map(|o| (b.slot, o)))
                .collect();
            let current_order: Vec<ObjectId> = staying.iter().map(|(_, o)| *o).collect();
            staying.sort();
            let file_order: Vec<ObjectId> = staying.iter().map(|(_, o)| *o).collect();

            // Moved to another page: out of this page's array.
            for a in self.iter() {
                if let Some(base) = &a.baseline {
                    if base.page == page && a.page != page {
                        if let Some(object) = base.object {
                            edits.delete_annotation(page, object);
                        }
                    }
                }
            }

            let is_incoming = |a: &Annotation| a.baseline.as_ref().is_none_or(|b| b.page != page);
            let on_page: Vec<&Annotation> = self.iter().filter(|a| a.page == page).collect();
            let first_incoming = on_page.iter().position(|a| is_incoming(a));
            let last_staying = on_page.iter().rposition(|a| !is_incoming(a));
            let interleaved = matches!((first_incoming, last_staying), (Some(i), Some(s)) if i < s);
            let reorder = file_order != current_order || interleaved;
            if reorder {
                for object in &file_order {
                    edits.delete_annotation(page, *object);
                }
                reordered_pages.push(page);
            }
            for a in on_page {
                let incoming = is_incoming(a);
                if incoming || reorder {
                    match objects.get(&a.id) {
                        Some(object) => edits.attach_annotation(page, *object),
                        // Written directly in the array: it stays where it is.
                        None if !incoming => {}
                        None => return Err(AnnotError::NotEditable(a.id.clone())),
                    }
                }
                for member in count_members.get(&a.id).into_iter().flatten() {
                    edits.attach_annotation(page, *member);
                }
            }
        }

        Ok(SavePlan {
            edits,
            added,
            modified,
            deleted,
            reordered_pages,
        })
    }
}
