//! Page scales and viewports: the page's `/VP` array.
//!
//! ISO 32000 (12.9.1) lets a page carry an array of viewports, rectangles of the page each with
//! a measure dictionary of its own: a detail drawn at another scale than the sheet. A point
//! that lies in several viewports belongs to the **last** one in the array.
//!
//! There is no standard key for the scale of a whole page, so this crate writes it as the
//! **first** viewport of the array, covering the page's `/MediaBox` and named
//! [`PAGE_SCALE_NAME`], and marks it in the private dictionary. Another application sees an
//! ordinary viewport over the whole page, which every later viewport overrides inside its own
//! box: exactly what the page scale means.
//!
//! [`PageViewports::load`] reads a page's array; [`PageViewports::plan`] writes it back as a
//! patch of the page dictionary that names `/VP` only, so every other page key is carried over
//! untouched, and only when something changed. Entries this crate cannot read (no `/BBox`,
//! say) are kept as the file has them, ahead of the others.

use pl_geom::{Point, Rect};
use pl_pdf::{Dictionary, EditSet, Object, PdfDocument};

use crate::measure::{MeasureSpec, ScaleRef};
use crate::measure_io::{apply_exact_factors, measure_dict, read_spec, set_exact_factors};
use crate::model::{Annotation, MODEL_KEY, PRIVATE_KEY};
use crate::objects::{encode_text, lookup, name, name_of, number_array, nums, text_of};
use crate::write::AnnotError;

/// The `/Name` of the viewport that holds the page's own scale.
pub const PAGE_SCALE_NAME: &str = "Page";

/// Why a viewport could not be added or changed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ViewportError {
    #[error("a viewport needs a name")]
    EmptyName,
    #[error("the name {0:?} is taken by another viewport of the page")]
    DuplicateName(String),
    #[error("a viewport's box must be finite and have an area")]
    BadBox,
}

/// One viewport: a named region of the page with a scale of its own.
#[derive(Debug, Clone, PartialEq)]
pub struct Viewport {
    /// `/Name`: how measurements refer to it ([`ScaleRef::Viewport`]).
    pub name: String,
    /// `/BBox`, in unrotated page space.
    pub bbox: Rect,
    /// `/Measure`; `None` for a viewport that only names a region.
    pub measure: Option<MeasureSpec>,
    /// The entry as the file holds it, so that a viewport that is not changed is written back
    /// as it was and the keys this crate does not model survive a change.
    original: Option<Object>,
    /// The entry's dictionary as read (resolved when the entry is a reference): a changed
    /// viewport is written from it, so the keys this crate does not model survive.
    original_dict: Option<Dictionary>,
    /// What `name`, `bbox` and `measure` were when read.
    read_as: Option<(String, Rect, Option<MeasureSpec>)>,
}

impl Viewport {
    pub fn new(name: impl Into<String>, bbox: Rect, measure: Option<MeasureSpec>) -> Viewport {
        Viewport {
            name: name.into(),
            bbox: bbox.abs(),
            measure,
            original: None,
            original_dict: None,
            read_as: None,
        }
    }

    /// True when `p` is inside the box or on its edge.
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.bbox.x0 && p.x <= self.bbox.x1 && p.y >= self.bbox.y0 && p.y <= self.bbox.y1
    }

    fn unchanged(&self) -> bool {
        self.read_as.as_ref().is_some_and(|(name, bbox, measure)| {
            *name == self.name && near_rect(*bbox, self.bbox) && *measure == self.measure
        })
    }
}

fn near_rect(a: Rect, b: Rect) -> bool {
    let n = |x: f64, y: f64| (x - y).abs() <= 0.002;
    n(a.x0, b.x0) && n(a.y0, b.y0) && n(a.x1, b.x1) && n(a.y1, b.y1)
}

/// The scales of one page: its own scale and its viewports, in `/VP` order.
#[derive(Debug, Clone, PartialEq)]
pub struct PageViewports {
    pub page: usize,
    /// The scale of the page outside every viewport.
    pub page_scale: Option<MeasureSpec>,
    /// The viewports, later ones on top.
    pub viewports: Vec<Viewport>,
    /// The page's `/MediaBox`, which the page-scale entry covers.
    media_box: Rect,
    /// Entries that could not be read, kept as they are.
    kept: Vec<Object>,
    /// The page-scale entry as read: the scale and the entry itself.
    page_entry: Option<(Option<MeasureSpec>, Object)>,
    /// The names of the viewports as read, in order.
    read_order: Vec<String>,
}

/// The exact factors of a viewport's scale, kept in its private dictionary.
fn private_part(spec: Option<&MeasureSpec>, kind: &str) -> Object {
    let mut private = Dictionary::new();
    private.set(MODEL_KEY, own_model(spec, kind));
    Object::Dictionary(private)
}

/// This crate's model dictionary of a viewport entry.
fn own_model(spec: Option<&MeasureSpec>, kind: &str) -> Object {
    let mut own = Dictionary::new();
    own.set("Type", name(kind));
    if let Some(spec) = spec {
        set_exact_factors(&mut own, spec);
    }
    Object::Dictionary(own)
}

/// This crate's private model dictionary of a viewport entry, if it has one.
fn own_part<'a>(doc: &'a PdfDocument, entry: &'a Dictionary) -> Option<&'a Dictionary> {
    lookup(doc, entry, PRIVATE_KEY.as_bytes())
        .and_then(|o| o.as_dict().ok())
        .and_then(|p| lookup(doc, p, MODEL_KEY.as_bytes()))
        .and_then(|o| o.as_dict().ok())
}

impl PageViewports {
    /// A page with no scale and no viewports. `media_box` is the page's `/MediaBox`.
    pub fn new(page: usize, media_box: Rect) -> PageViewports {
        PageViewports {
            page,
            page_scale: None,
            viewports: Vec::new(),
            media_box,
            kept: Vec::new(),
            page_entry: None,
            read_order: Vec::new(),
        }
    }

    /// Reads the `/VP` array of a page. A page without one (or one that cannot be read) gives
    /// an empty set; nothing here fails on a malformed entry.
    pub fn load(doc: &PdfDocument, page: usize) -> Result<PageViewports, AnnotError> {
        let page_count = doc.page_count();
        let info = doc.page(page).map_err(|_| AnnotError::PageOutOfRange {
            id: crate::AnnotId(String::new()),
            page,
            count: page_count,
        })?;
        let mut out = PageViewports::new(page, info.media_box);
        let Some(Object::Dictionary(page_dict)) = doc.object(info.id) else {
            return Ok(out);
        };
        let Some(array) = lookup(doc, page_dict, b"VP").and_then(|o| o.as_array().ok()) else {
            return Ok(out);
        };
        for (i, item) in array.iter().enumerate() {
            let entry = match item {
                Object::Reference(_) => doc.resolve(item),
                other => Some(other),
            }
            .and_then(|o| o.as_dict().ok());
            let bbox = entry
                .and_then(|e| lookup(doc, e, b"BBox"))
                .and_then(|o| number_array(doc, o))
                .filter(|v| v.len() == 4 && v.iter().all(|x| x.is_finite()))
                .map(|v| Rect::new(v[0], v[1], v[2], v[3]).abs());
            let (Some(entry), Some(bbox)) = (entry, bbox) else {
                out.kept.push(item.clone());
                continue;
            };
            let own = own_part(doc, entry);
            let mut spec = lookup(doc, entry, b"Measure")
                .and_then(|o| o.as_dict().ok())
                .and_then(|d| read_spec(doc, d));
            if let (Some(spec), Some(own)) = (spec.as_mut(), own) {
                apply_exact_factors(doc, spec, own);
            }
            let own_type = own.and_then(|o| lookup(doc, o, b"Type")).and_then(name_of);
            if i == 0 && own_type.as_deref() == Some("PageScale") {
                out.page_scale = spec.clone();
                out.page_entry = Some((spec, item.clone()));
                continue;
            }
            let name = lookup(doc, entry, b"Name")
                .and_then(text_of)
                .unwrap_or_default();
            out.read_order.push(name.clone());
            out.viewports.push(Viewport {
                read_as: Some((name.clone(), bbox, spec.clone())),
                name,
                bbox,
                measure: spec,
                original: Some(item.clone()),
                original_dict: Some(entry.clone()),
            });
        }
        Ok(out)
    }

    /// Adds a viewport on top of the others. Its name must be new on the page.
    pub fn add(&mut self, viewport: Viewport) -> Result<(), ViewportError> {
        self.check(&viewport, None)?;
        self.viewports.push(viewport);
        Ok(())
    }

    fn check(&self, v: &Viewport, except: Option<usize>) -> Result<(), ViewportError> {
        if v.name.trim().is_empty() {
            return Err(ViewportError::EmptyName);
        }
        let b = v.bbox;
        if ![b.x0, b.y0, b.x1, b.y1].iter().all(|x| x.is_finite()) || b.area() <= 0.0 {
            return Err(ViewportError::BadBox);
        }
        let taken = v.name == PAGE_SCALE_NAME
            || self
                .viewports
                .iter()
                .enumerate()
                .any(|(i, o)| Some(i) != except && o.name == v.name);
        if taken {
            return Err(ViewportError::DuplicateName(v.name.clone()));
        }
        Ok(())
    }

    /// The viewport of this name.
    pub fn get(&self, name: &str) -> Option<&Viewport> {
        self.viewports.iter().find(|v| v.name == name)
    }

    /// Changes the box and the scale of a viewport. Returns false when there is none of that
    /// name; an error when the new box is not usable.
    pub fn edit(
        &mut self,
        name: &str,
        bbox: Rect,
        measure: Option<MeasureSpec>,
    ) -> Result<bool, ViewportError> {
        let Some(i) = self.viewports.iter().position(|v| v.name == name) else {
            return Ok(false);
        };
        let mut v = self.viewports[i].clone();
        v.bbox = bbox.abs();
        v.measure = measure;
        self.check(&v, Some(i))?;
        self.viewports[i] = v;
        Ok(true)
    }

    /// Takes a viewport off the page.
    pub fn remove(&mut self, name: &str) -> Option<Viewport> {
        let i = self.viewports.iter().position(|v| v.name == name)?;
        Some(self.viewports.remove(i))
    }

    /// The topmost viewport with a scale whose box holds every one of `points`.
    pub fn viewport_for(&self, points: &[Point]) -> Option<&Viewport> {
        if points.is_empty() {
            return None;
        }
        self.viewports
            .iter()
            .rev()
            .filter(|v| v.measure.is_some())
            .find(|v| points.iter().all(|p| v.contains(*p)))
    }

    /// The scale that applies to a measurement over `points`: the topmost viewport that holds
    /// them all, otherwise the page's own.
    pub fn scale_for(&self, points: &[Point]) -> (ScaleRef, Option<&MeasureSpec>) {
        match self.viewport_for(points) {
            Some(v) => (ScaleRef::Viewport(v.name.clone()), v.measure.as_ref()),
            None => (ScaleRef::Page, self.page_scale.as_ref()),
        }
    }

    /// The scale a [`ScaleRef`] names; `None` when the page or viewport has none (or there is
    /// no such viewport).
    pub fn spec(&self, scale: &ScaleRef) -> Option<&MeasureSpec> {
        match scale {
            ScaleRef::Page => self.page_scale.as_ref(),
            ScaleRef::Viewport(name) => self.get(name).and_then(|v| v.measure.as_ref()),
        }
    }

    /// Puts a measurement on the scale that applies where it is (see
    /// [`PageViewports::scale_for`]). Returns false, changing nothing, for an annotation that
    /// is not a measurement or a place with no scale.
    pub fn apply_to(&self, a: &mut Annotation) -> bool {
        if a.measure.is_none() {
            return false;
        }
        let mut points = Vec::new();
        a.geometry.for_each_point(|p| points.push(p));
        let (scale, spec) = self.scale_for(&points);
        match spec {
            Some(spec) => {
                a.apply_scale(scale, spec.clone());
                true
            }
            None => false,
        }
    }

    /// Brings a measurement in step with the scale it refers to, after that scale was edited.
    /// Returns false, changing nothing, when the scale it names is gone or has no measure.
    pub fn refresh(&self, a: &mut Annotation) -> bool {
        let Some(scale) = a.measure.as_ref().map(|m| m.scale.clone()) else {
            return false;
        };
        match self.spec(&scale) {
            Some(spec) => {
                a.apply_scale(scale, spec.clone());
                true
            }
            None => false,
        }
    }

    /// True when a save would write the page's `/VP`.
    pub fn is_dirty(&self) -> bool {
        self.page_scale_changed()
            || self.viewports.iter().any(|v| !v.unchanged())
            || !self
                .viewports
                .iter()
                .map(|v| &v.name)
                .eq(self.read_order.iter())
    }

    fn page_scale_changed(&self) -> bool {
        match &self.page_entry {
            Some((read, _)) => *read != self.page_scale,
            None => self.page_scale.is_some(),
        }
    }

    /// The array entries a save would write, in order.
    fn entries(&self) -> Option<Vec<Object>> {
        let mut out: Vec<Object> = Vec::new();
        if let Some(spec) = &self.page_scale {
            match &self.page_entry {
                Some((read, original)) if read.as_ref() == Some(spec) => out.push(original.clone()),
                _ => {
                    let mut d = Dictionary::new();
                    d.set("Type", name("Viewport"));
                    let m = self.media_box;
                    d.set("BBox", nums(&[m.x0, m.y0, m.x1, m.y1]));
                    d.set("Name", encode_text(PAGE_SCALE_NAME));
                    d.set("Measure", Object::Dictionary(measure_dict(spec)));
                    d.set(PRIVATE_KEY, private_part(Some(spec), "PageScale"));
                    out.push(Object::Dictionary(d));
                }
            }
        }
        out.extend(self.kept.iter().cloned());
        for v in &self.viewports {
            match (&v.original, v.unchanged()) {
                (Some(original), true) => out.push(original.clone()),
                _ => out.push(Object::Dictionary(self.viewport_dict(v))),
            }
        }
        (!out.is_empty()).then_some(out)
    }

    fn viewport_dict(&self, v: &Viewport) -> Dictionary {
        // Start from the entry as the file has it (resolved, when the entry is a reference),
        // so keys this crate does not model survive.
        let mut d = v.original_dict.clone().unwrap_or_default();
        d.set("Type", name("Viewport"));
        let b = v.bbox;
        d.set("BBox", nums(&[b.x0, b.y0, b.x1, b.y1]));
        d.set("Name", encode_text(&v.name));
        // Only this crate's model part of the private dictionary is ours to replace.
        let mut private = match d.get(PRIVATE_KEY.as_bytes()) {
            Ok(Object::Dictionary(p)) => p.clone(),
            _ => Dictionary::new(),
        };
        let read_a_scale = v.read_as.as_ref().is_some_and(|(_, _, m)| m.is_some());
        match &v.measure {
            Some(spec) => {
                d.set("Measure", Object::Dictionary(measure_dict(spec)));
                private.set(MODEL_KEY, own_model(Some(spec), "Viewport"));
            }
            None => {
                // A scale the user took off goes; a /Measure this crate cannot read (a
                // geospatial one) was never in the model and stays as it is.
                if read_a_scale || v.original_dict.is_none() {
                    d.remove(b"Measure");
                }
                private.remove(MODEL_KEY.as_bytes());
            }
        }
        if private.is_empty() {
            d.remove(PRIVATE_KEY.as_bytes());
        } else {
            d.set(PRIVATE_KEY, Object::Dictionary(private));
        }
        d
    }

    /// Adds to `edits` what makes the page hold these viewports: a patch of the page
    /// dictionary that sets (or removes) `/VP` and nothing else. Returns false, adding nothing,
    /// when the page already holds them.
    pub fn plan(&self, doc: &PdfDocument, edits: &mut EditSet) -> Result<bool, AnnotError> {
        if !self.is_dirty() {
            return Ok(false);
        }
        let page_id = doc
            .page_id(self.page)
            .map_err(|_| AnnotError::PageOutOfRange {
                id: crate::AnnotId(String::new()),
                page: self.page,
                count: doc.page_count(),
            })?;
        let patch = edits.patch(page_id);
        match self.entries() {
            Some(entries) => {
                patch.set("VP", Object::Array(entries));
            }
            None => {
                patch.remove("VP");
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measure::MeasureFormat;
    use pl_geom::{AreaFormat, AreaUnit, LengthFormat, Scale, Unit};

    fn spec(feet: f64) -> MeasureSpec {
        MeasureSpec::new(
            &Scale::engineering(feet).unwrap(),
            1.0,
            &MeasureFormat::new(
                LengthFormat::decimal(Unit::Foot, 2),
                AreaFormat::new(AreaUnit::SqFoot, 2),
            ),
        )
    }

    #[test]
    fn the_last_viewport_that_holds_every_point_wins() {
        let mut vps = PageViewports::new(0, Rect::new(0.0, 0.0, 612.0, 792.0));
        vps.page_scale = Some(spec(20.0));
        vps.add(Viewport::new(
            "Big",
            Rect::new(100.0, 100.0, 400.0, 400.0),
            Some(spec(10.0)),
        ))
        .unwrap();
        vps.add(Viewport::new(
            "Small",
            Rect::new(200.0, 200.0, 300.0, 300.0),
            Some(spec(5.0)),
        ))
        .unwrap();
        // A region with no scale names a place but never decides one.
        vps.add(Viewport::new(
            "Label",
            Rect::new(0.0, 0.0, 612.0, 792.0),
            None,
        ))
        .unwrap();
        let p = |x: f64, y: f64| Point::new(x, y);
        fn name(vps: &PageViewports, points: &[Point]) -> String {
            match vps.scale_for(points).0 {
                ScaleRef::Page => "page".to_string(),
                ScaleRef::Viewport(n) => n,
            }
        }
        assert_eq!(name(&vps, &[p(250.0, 250.0)]), "Small");
        assert_eq!(name(&vps, &[p(150.0, 150.0)]), "Big");
        assert_eq!(
            name(&vps, &[p(250.0, 250.0), p(350.0, 350.0)]),
            "Big",
            "not all in Small"
        );
        assert_eq!(name(&vps, &[p(250.0, 250.0), p(500.0, 500.0)]), "page");
        assert_eq!(name(&vps, &[p(400.0, 400.0)]), "Big", "the edge is inside");
        assert_eq!(name(&vps, &[]), "page");
        assert_eq!(
            vps.spec(&ScaleRef::Viewport("Small".into())),
            Some(&spec(5.0))
        );
        assert_eq!(vps.spec(&ScaleRef::Viewport("Gone".into())), None);
        assert_eq!(vps.spec(&ScaleRef::Page), Some(&spec(20.0)));

        assert_eq!(
            vps.edit("Small", Rect::new(1.0, 1.0, 1.0, 9.0), None),
            Err(ViewportError::BadBox)
        );
        assert_eq!(
            vps.edit("Small", Rect::new(1.0, 1.0, 9.0, 9.0), None)
                .map(|_| ()),
            Ok(())
        );
        assert_eq!(
            vps.add(Viewport::new("Big", Rect::new(0.0, 0.0, 1.0, 1.0), None)),
            Err(ViewportError::DuplicateName("Big".into()))
        );
        assert!(vps.remove("Big").is_some() && vps.remove("Big").is_none());
        assert_eq!(name(&vps, &[p(150.0, 150.0)]), "page");
        assert!(vps.is_dirty());
    }
}
