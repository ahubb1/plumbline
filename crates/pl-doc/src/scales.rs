//! The scale state of a document: every page's own scale and viewports, and the document-wide
//! settings (a default scale and the takeoff definitions: legends, spaces, sequences).
//!
//! # Where it is stored
//!
//! * A page's scale and viewports are its `/VP` array, read and written by pl-annot's
//!   [`PageViewports`] (the page's own scale is the first entry, covering the MediaBox).
//! * The document settings are app-only data: one JSON text under the private key
//!   ([`PRIVATE_KEY`](pl_annot::PRIVATE_KEY)) of the document catalog, as
//!   `/PlumblineData << /PlDocSettings (json) >>`. Other keys of that dictionary are kept.
//!
//! # Undo
//!
//! [`crate::Editor`] owns a [`ScaleStore`]. Its undo steps hold a page's scales as plain data
//! ([`PageScalesData`]) and turn them back into a [`PageViewports`] against the page as last
//! read from the file ([`ScaleStore::set_page_data`]), so a viewport that came from the file
//! keeps its tie to its original entry, and an undo after a save is written like any change.

use pl_annot::{MeasureSpec, PageViewports, ScaleRef, Viewport, PRIVATE_KEY};
use pl_geom::{Point, Rect};
use pl_pdf::{Dictionary, EditSet, Object, PdfDocument, StringFormat};
use pl_takeoff::{BluebeamColumns, ANNOT_COLUMNS_KEY};
use serde::{Deserialize, Serialize};

use crate::measure_data::{MeasureSpecData, PageScalesData, ScaleSetting, ViewportData};

/// The key of the document settings inside the catalog's private dictionary.
pub const SETTINGS_KEY: &str = "PlDocSettings";

/// A legend: a box on a page that lists the markups (or measurements) of a document by
/// subject, with their totals. Its contents are worked out by pl-takeoff (see
/// [`TakeoffRefresh`]); this is only its definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LegendDef {
    pub id: String,
    pub title: String,
    /// Zero-based page the legend is drawn on.
    pub page: usize,
    /// `[x0, y0, x1, y1]`, unrotated page space.
    pub rect: [f64; 4],
    /// Only markups whose subject is one of these; empty for every subject.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subjects: Vec<String>,
    /// The list columns shown, by column id (`subject`, `count`, `length`, ...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<String>,
}

/// A space: a named region of a page (a room, a zone). Markups inside it are counted to it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpaceDef {
    pub id: String,
    pub name: String,
    pub page: usize,
    /// The outline, unrotated page space, at least three points.
    pub outline: Vec<[f64; 2]>,
    /// The space this one lies in, by id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

impl SpaceDef {
    /// Whether `p` on `page` is inside the space (on the edge counts as inside).
    pub fn contains(&self, page: usize, p: Point) -> bool {
        if page != self.page {
            return false;
        }
        let outline: Vec<Point> = self
            .outline
            .iter()
            .map(|q| Point::new(q[0], q[1]))
            .collect();
        pl_geom::point_in_polygon(p, &outline)
            || outline
                .iter()
                .zip(outline.iter().cycle().skip(1))
                .any(|(a, b)| pl_geom::point_segment_distance(p, *a, *b) <= 1e-9)
    }
}

/// A sequence: numbers handed out one after another to label markups (`D-001`, `D-002`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SequenceDef {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub suffix: String,
    /// The number the next label gets.
    pub next: i64,
    /// What is added after each label; never 0.
    pub step: i64,
    /// The number is padded with zeros to this many digits.
    #[serde(default)]
    pub digits: u8,
}

impl SequenceDef {
    /// The label of number `n`: `D-007` for prefix `D-`, three digits.
    pub fn label(&self, n: i64) -> String {
        let width = self.digits as usize;
        let digits = if n < 0 {
            format!("-{:0width$}", n.unsigned_abs())
        } else {
            format!("{n:0width$}")
        };
        format!("{}{digits}{}", self.prefix, self.suffix)
    }
}

/// The document-wide settings, stored in the catalog's private dictionary.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct DocSettings {
    /// The scale of a page that has none of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_scale: Option<ScaleSetting>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub legends: Vec<LegendDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spaces: Vec<SpaceDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sequences: Vec<SequenceDef>,
    /// The units the takeoff's quantity columns (and so its formulas and Wall Area heights)
    /// are in. When `None`, the app chooses them once per session from the measurements and
    /// keeps them, so adding measurements of the other system never changes their meaning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_units: Option<pl_takeoff::QuantityUnits>,
}

impl DocSettings {
    pub fn is_empty(&self) -> bool {
        *self == DocSettings::default()
    }

    /// Checks the definitions against a document of `page_count` pages: ids unique within
    /// their list, pages in range, boxes and outlines usable, sequence steps not zero.
    pub fn validate(&self, page_count: usize) -> Result<(), String> {
        fn unique<'a>(what: &str, ids: impl Iterator<Item = &'a str>) -> Result<(), String> {
            let mut seen = std::collections::HashSet::new();
            for id in ids {
                if id.trim().is_empty() {
                    return Err(format!("a {what} needs an id"));
                }
                if !seen.insert(id) {
                    return Err(format!("two {what}s have the id {id:?}"));
                }
            }
            Ok(())
        }
        let page_ok = |what: &str, id: &str, page: usize| {
            if page < page_count {
                Ok(())
            } else {
                Err(format!(
                    "{what} {id:?} is on page {}, but the document has {page_count} pages",
                    page + 1
                ))
            }
        };
        unique("legend", self.legends.iter().map(|l| l.id.as_str()))?;
        unique("space", self.spaces.iter().map(|s| s.id.as_str()))?;
        unique("sequence", self.sequences.iter().map(|s| s.id.as_str()))?;
        if let Some(scale) = &self.default_scale {
            scale.spec(1.0)?;
        }
        for l in &self.legends {
            page_ok("legend", &l.id, l.page)?;
            let r = Rect::new(l.rect[0], l.rect[1], l.rect[2], l.rect[3]);
            if !l.rect.iter().all(|v| v.is_finite()) || r.abs().area() <= 0.0 {
                return Err(format!("legend {:?} needs a box with an area", l.id));
            }
        }
        for s in &self.spaces {
            page_ok("space", &s.id, s.page)?;
            let finite = s.outline.iter().flatten().all(|v| v.is_finite());
            if s.outline.len() < 3 || !finite {
                return Err(format!(
                    "space {:?} needs an outline of at least three points",
                    s.id
                ));
            }
            if let Some(parent) = &s.parent {
                if parent == &s.id || !self.spaces.iter().any(|o| &o.id == parent) {
                    return Err(format!(
                        "space {:?} names {parent:?} as its parent, which is not another space",
                        s.id
                    ));
                }
            }
        }
        for s in &self.sequences {
            if s.step == 0 {
                return Err(format!("sequence {:?} has a step of 0", s.id));
            }
        }
        Ok(())
    }

    /// The innermost space holding `p` on `page`: of the spaces that contain it, the one with
    /// the smallest area.
    pub fn space_at(&self, page: usize, p: Point) -> Option<&SpaceDef> {
        let area = |s: &SpaceDef| {
            let pts: Vec<Point> = s.outline.iter().map(|q| Point::new(q[0], q[1])).collect();
            pl_geom::polygon_area(&pts).abs()
        };
        self.spaces
            .iter()
            .filter(|s| s.contains(page, p))
            .min_by(|a, b| area(a).total_cmp(&area(b)))
    }
}

/// What has changed since a user of the takeoff definitions last looked
/// ([`crate::Editor::take_takeoff_refresh`]).
///
/// **Seam for pl-takeoff:** legends, spaces and sequences are evaluated by pl-takeoff, which
/// is being written alongside this crate. Until its evaluation API exists, a user interface
/// polls this after edits and re-runs whatever shows legend text or space totals. When the
/// pl-takeoff API lands, the call goes where [`TakeoffRefresh`] is consumed.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TakeoffRefresh {
    /// Pages whose markups or scales changed.
    pub pages: std::collections::BTreeSet<usize>,
    /// The legend, space or sequence definitions (or the default scale) changed.
    pub definitions: bool,
}

impl TakeoffRefresh {
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty() && !self.definitions
    }
}

/// Every page's scales and the document settings, with what the file holds for them.
#[derive(Clone, Debug, PartialEq)]
pub struct ScaleStore {
    pages: Vec<PageViewports>,
    /// The pages as last read from the file.
    baseline: Vec<PageViewports>,
    user_units: Vec<f64>,
    settings: DocSettings,
    /// The settings as last read from the file.
    settings_read: DocSettings,
    /// The catalog's private dictionary as the file holds it.
    catalog_private: Option<Dictionary>,
    /// Bluebeam Revu's custom column definitions (`/BSIAnnotColumns`), and as the file holds
    /// them.
    bluebeam: BluebeamColumns,
    bluebeam_read: BluebeamColumns,
}

/// A page's scales as plain data.
pub fn page_data(vps: &PageViewports) -> PageScalesData {
    PageScalesData {
        page_scale: vps.page_scale.as_ref().map(MeasureSpecData::from),
        viewports: vps
            .viewports
            .iter()
            .map(|v| ViewportData {
                name: v.name.clone(),
                bbox: [v.bbox.x0, v.bbox.y0, v.bbox.x1, v.bbox.y1],
                spec: v.measure.as_ref().map(MeasureSpecData::from),
            })
            .collect(),
    }
}

/// The catalog's private dictionary, resolved.
fn catalog_private(doc: &PdfDocument) -> Option<Dictionary> {
    let catalog = doc.catalog().ok()?;
    let value = catalog.get(PRIVATE_KEY.as_bytes()).ok()?;
    doc.resolve(value)?.as_dict().ok().cloned()
}

fn read_settings(private: Option<&Dictionary>) -> Option<DocSettings> {
    let value = private?.get(SETTINGS_KEY.as_bytes()).ok()?;
    let Object::String(bytes, _) = value else {
        return None;
    };
    serde_json::from_slice(bytes).ok()
}

impl ScaleStore {
    /// A store with no scales for `page_count` pages of `/UserUnit` 1 and unknown size: what
    /// a document gets whose file cannot be read.
    pub fn empty(page_count: usize) -> ScaleStore {
        ScaleStore::blank(&vec![(Rect::ZERO, 1.0); page_count])
    }

    /// A store with no scales for pages of the given MediaBox and `/UserUnit`.
    pub fn blank(pages: &[(Rect, f64)]) -> ScaleStore {
        let vps: Vec<PageViewports> = pages
            .iter()
            .enumerate()
            .map(|(i, (media, _))| PageViewports::new(i, *media))
            .collect();
        ScaleStore {
            baseline: vps.clone(),
            pages: vps,
            user_units: pages.iter().map(|(_, u)| *u).collect(),
            settings: DocSettings::default(),
            settings_read: DocSettings::default(),
            catalog_private: None,
            bluebeam: BluebeamColumns::default(),
            bluebeam_read: BluebeamColumns::default(),
        }
    }

    /// Reads every page's `/VP` and the document settings. Never fails: what cannot be read
    /// is left out (and an unreadable `/VP` entry is kept as the file has it).
    pub fn load(doc: &PdfDocument) -> ScaleStore {
        let count = doc.page_count();
        let mut pages = Vec::with_capacity(count);
        let mut units = Vec::with_capacity(count);
        for i in 0..count {
            let info = doc.page(i).ok();
            units.push(info.as_ref().map_or(1.0, |p| p.user_unit));
            let vps = PageViewports::load(doc, i).unwrap_or_else(|_| {
                PageViewports::new(i, info.as_ref().map_or(Rect::ZERO, |p| p.media_box))
            });
            pages.push(vps);
        }
        let private = catalog_private(doc);
        let settings = read_settings(private.as_ref()).unwrap_or_default();
        let bluebeam = BluebeamColumns::read(doc);
        ScaleStore {
            baseline: pages.clone(),
            pages,
            user_units: units,
            settings_read: settings.clone(),
            settings,
            catalog_private: private,
            bluebeam_read: bluebeam.clone(),
            bluebeam,
        }
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// A page's scales; `None` for a page the document does not have.
    pub fn page(&self, page: usize) -> Option<&PageViewports> {
        self.pages.get(page)
    }

    /// `/UserUnit` of a page (1 for a page the store does not know).
    pub fn user_unit(&self, page: usize) -> f64 {
        self.user_units.get(page).copied().unwrap_or(1.0)
    }

    pub fn settings(&self) -> &DocSettings {
        &self.settings
    }

    /// Grows or shrinks the store to `count` pages (new pages have no scale).
    pub(crate) fn resize(&mut self, count: usize) {
        while self.pages.len() < count {
            let i = self.pages.len();
            self.pages.push(PageViewports::new(i, Rect::ZERO));
            self.baseline.push(PageViewports::new(i, Rect::ZERO));
            self.user_units.push(1.0);
        }
        self.pages.truncate(count);
        self.baseline.truncate(count);
        self.user_units.truncate(count);
    }

    /// A page's scales as plain data.
    pub fn page_data(&self, page: usize) -> PageScalesData {
        self.pages.get(page).map(page_data).unwrap_or_default()
    }

    /// Every page's scales as plain data: what two states are compared by.
    pub fn data(&self) -> Vec<PageScalesData> {
        self.pages.iter().map(page_data).collect()
    }

    /// The page's scales as `data` says, built on the page as the file has it, so viewports
    /// read from the file keep their original entries.
    pub(crate) fn materialise(&self, page: usize, data: &PageScalesData) -> PageViewports {
        let base = match self.baseline.get(page) {
            Some(b) => b.clone(),
            None => PageViewports::new(page, Rect::ZERO),
        };
        let mut out = base.clone();
        out.page_scale = data.page_scale.as_ref().map(MeasureSpecData::to_spec);
        out.viewports = data
            .viewports
            .iter()
            .map(|d| {
                let spec = d.spec.as_ref().map(MeasureSpecData::to_spec);
                match base.get(&d.name) {
                    Some(original) => {
                        let mut v = original.clone();
                        v.bbox = d.rect();
                        v.measure = spec;
                        v
                    }
                    None => Viewport::new(d.name.clone(), d.rect(), spec),
                }
            })
            .collect();
        out
    }

    /// Sets a page's scales from plain data.
    pub(crate) fn set_page_data(&mut self, page: usize, data: &PageScalesData) {
        if page >= self.pages.len() {
            self.resize(page + 1);
        }
        self.pages[page] = self.materialise(page, data);
    }

    pub(crate) fn set_settings(&mut self, settings: DocSettings) {
        self.settings = settings;
    }

    /// Bluebeam Revu's custom column definitions (`/BSIAnnotColumns`).
    pub fn bluebeam(&self) -> &BluebeamColumns {
        &self.bluebeam
    }

    /// Sets the entries of `/BSIAnnotColumns`.
    pub(crate) fn set_bluebeam_entries(&mut self, entries: Vec<Object>) {
        self.bluebeam.entries = entries;
    }

    pub fn bluebeam_dirty(&self) -> bool {
        self.bluebeam.entries != self.bluebeam_read.entries
    }

    /// The scale that applies to a measurement over `points` on `page`: the topmost viewport
    /// with a scale that holds them all, else the page's own scale, else the document's
    /// default scale (built for the page's `/UserUnit`). `None` when there is none.
    pub fn resolve(&self, page: usize, points: &[Point]) -> Option<(ScaleRef, MeasureSpec)> {
        let vps = self.pages.get(page)?;
        match vps.scale_for(points) {
            (scale, Some(spec)) => Some((scale, spec.clone())),
            (ScaleRef::Page, None) => {
                let setting = self.settings.default_scale.as_ref()?;
                Some((ScaleRef::Page, setting.spec(self.user_unit(page)).ok()?))
            }
            (ScaleRef::Viewport(_), None) => None,
        }
    }

    /// The pages whose `/VP` a save would write.
    pub fn dirty_pages(&self) -> Vec<usize> {
        self.pages
            .iter()
            .enumerate()
            .filter(|(_, p)| p.is_dirty())
            .map(|(i, _)| i)
            .collect()
    }

    pub fn settings_dirty(&self) -> bool {
        self.settings != self.settings_read
    }

    /// True when a save would write anything of the store.
    pub fn is_dirty(&self) -> bool {
        self.settings_dirty()
            || self.bluebeam_dirty()
            || self.pages.iter().any(PageViewports::is_dirty)
    }

    /// Adds to `edits` what makes the file hold this state: a patch of the `/VP` of each
    /// changed page and of the catalog's private dictionary. Returns the pages whose `/VP` is
    /// written. Adds nothing when nothing changed.
    pub fn plan(
        &self,
        doc: &PdfDocument,
        edits: &mut EditSet,
    ) -> Result<Vec<usize>, pl_annot::AnnotError> {
        let mut pages = Vec::new();
        for vps in &self.pages {
            if vps.page < doc.page_count() && vps.plan(doc, edits)? {
                pages.push(vps.page);
            }
        }
        if self.settings_dirty() {
            self.plan_settings(doc, edits);
        }
        if self.bluebeam_dirty() {
            self.plan_bluebeam(doc, edits);
        }
        Ok(pages)
    }

    /// Writes `/BSIAnnotColumns`: into its own object when the catalog refers to one (the
    /// reference is left as it is), else as a new object the catalog refers to. Revu reads
    /// only that indirect form, so a direct array in the file becomes indirect.
    fn plan_bluebeam(&self, doc: &PdfDocument, edits: &mut EditSet) {
        let array = self.bluebeam.to_object();
        match self.bluebeam_read.object {
            Some(id) => edits.set_object(id, array),
            None => {
                let Some(root) = doc.trailer().get(b"Root").ok().and_then(|o| match o {
                    Object::Reference(id) => Some(*id),
                    _ => None,
                }) else {
                    return;
                };
                let id = edits.add_object(array);
                edits
                    .patch(root)
                    .set(ANNOT_COLUMNS_KEY, Object::Reference(id));
            }
        }
    }

    fn plan_settings(&self, doc: &PdfDocument, edits: &mut EditSet) {
        let Some(root) = doc.trailer().get(b"Root").ok().and_then(|o| match o {
            Object::Reference(id) => Some(*id),
            _ => None,
        }) else {
            return;
        };
        let mut private = self.catalog_private.clone().unwrap_or_default();
        if self.settings.is_empty() {
            private.remove(SETTINGS_KEY.as_bytes());
        } else {
            let json = serde_json::to_vec(&self.settings).expect("settings serialise");
            private.set(SETTINGS_KEY, Object::String(json, StringFormat::Literal));
        }
        // An indirect private dictionary is changed where it is; a direct one with the catalog.
        let indirect = doc
            .catalog()
            .ok()
            .and_then(|c| c.get(PRIVATE_KEY.as_bytes()).ok())
            .and_then(|o| match o {
                Object::Reference(id) => Some(*id),
                _ => None,
            });
        match indirect {
            Some(id) => {
                edits.set_object(id, Object::Dictionary(private));
            }
            None if private.is_empty() => {
                edits.patch(root).remove(PRIVATE_KEY);
            }
            None => {
                edits
                    .patch(root)
                    .set(PRIVATE_KEY, Object::Dictionary(private));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_labels_and_space_lookup() {
        let seq = SequenceDef {
            id: "d".into(),
            name: "Doors".into(),
            prefix: "D-".into(),
            suffix: String::new(),
            next: 7,
            step: 1,
            digits: 3,
        };
        assert_eq!(seq.label(7), "D-007");
        assert_eq!(seq.label(1234), "D-1234");
        assert_eq!(seq.label(-2), "D--002");

        let room = |id: &str, x0: f64, x1: f64| SpaceDef {
            id: id.into(),
            name: id.to_uppercase(),
            page: 0,
            outline: vec![[x0, 0.0], [x1, 0.0], [x1, 100.0], [x0, 100.0]],
            parent: None,
        };
        let settings = DocSettings {
            spaces: vec![room("floor", 0.0, 400.0), room("kitchen", 100.0, 200.0)],
            ..DocSettings::default()
        };
        assert!(settings.validate(1).is_ok());
        let at = |x: f64| {
            settings
                .space_at(0, Point::new(x, 50.0))
                .map(|s| s.id.clone())
        };
        assert_eq!(at(150.0).as_deref(), Some("kitchen"), "the smaller one");
        assert_eq!(at(100.0).as_deref(), Some("kitchen"), "on its edge");
        assert_eq!(at(300.0).as_deref(), Some("floor"));
        assert_eq!(at(500.0), None);
        assert_eq!(settings.space_at(1, Point::new(150.0, 50.0)), None);

        let mut bad = settings.clone();
        bad.spaces[1].id = "floor".into();
        assert!(bad.validate(1).unwrap_err().contains("two spaces"));
        let mut bad = settings.clone();
        bad.spaces[0].page = 3;
        assert!(bad.validate(1).unwrap_err().contains("page 4"));
        let mut bad = settings.clone();
        bad.spaces[1].parent = Some("nowhere".into());
        assert!(bad.validate(1).is_err());
        let mut bad = settings;
        bad.sequences.push(SequenceDef { step: 0, ..seq });
        assert!(bad.validate(1).unwrap_err().contains("step of 0"));
    }

    #[test]
    fn materialised_data_equals_the_state_it_came_from() {
        let mut store = ScaleStore::blank(&[(Rect::new(0.0, 0.0, 612.0, 792.0), 2.0)]);
        let setting = ScaleSetting::parse("1:100", None).unwrap();
        let mut data = PageScalesData {
            page_scale: Some(MeasureSpecData::from(&setting.spec(2.0).unwrap())),
            viewports: vec![ViewportData {
                name: "Detail".into(),
                bbox: [10.0, 20.0, 110.0, 220.0],
                spec: None,
            }],
        };
        store.set_page_data(0, &data);
        assert_eq!(store.page_data(0), data);
        assert!(store.is_dirty() && store.dirty_pages() == [0]);
        assert_eq!(store.user_unit(0), 2.0);
        // A viewport with no scale never decides one; the page's does.
        let (scale, spec) = store.resolve(0, &[Point::new(50.0, 50.0)]).unwrap();
        assert_eq!(scale, ScaleRef::Page);
        assert_eq!(spec, setting.spec(2.0).unwrap());
        data.page_scale = None;
        store.set_page_data(0, &data);
        assert_eq!(store.resolve(0, &[Point::new(50.0, 50.0)]), None);
        store.set_page_data(0, &PageScalesData::default());
        assert!(!store.is_dirty(), "back to what the file has");
        // The default scale stands in for a page that has none.
        store.set_settings(DocSettings {
            default_scale: Some(setting.clone()),
            ..DocSettings::default()
        });
        assert_eq!(
            store.resolve(0, &[Point::new(1.0, 1.0)]),
            Some((ScaleRef::Page, setting.spec(2.0).unwrap()))
        );
        assert!(store.settings_dirty());
    }
}
