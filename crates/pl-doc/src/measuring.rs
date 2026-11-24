//! Scales, measurements, snapping and the takeoff definitions of a [`Document`].
//!
//! Each method that changes something builds a [`Command`] (see [`crate::measure_ops`] and
//! the scale commands of [`Command`]) and applies it: one undo step, journaled, and saved with
//! the document. A page's scale and viewports are written into its `/VP`, the document
//! settings into the catalog's private dictionary ([`crate::scales`]).

use pl_annot::{AnnotId, Geometry, MeasureFormat, Origin, PageViewports, PdfDate};
use pl_geom::{Point, Scale, Unit};

use crate::command::{Command, EditError, Outcome};
use crate::error::Result;
use crate::measure_data::ScaleSetting;
use crate::measure_ops::{self, MeasureShape, MeasurementRow};
use crate::scales::{DocSettings, LegendDef, ScaleStore, SequenceDef, SpaceDef, TakeoffRefresh};
use crate::session::Document;
use crate::snap::{self, SnapOptions, SnapOutcome};

impl Document {
    /// The pages' scales and viewports and the document settings, loading the annotation
    /// store first if need be.
    pub fn scales(&mut self) -> Result<&ScaleStore> {
        Ok(self.annots()?.scales())
    }

    /// One page's scale and viewports, if the store is loaded. Never loads, never waits.
    pub fn page_scales(&self, page: u32) -> Option<&PageViewports> {
        self.editor()?.scales().page(page as usize)
    }

    /// `/UserUnit` of a page as the display knows it.
    fn page_user_unit(&self, page: u32) -> f64 {
        self.pages()
            .get(page as usize)
            .map_or(1.0, |g| g.user_unit())
    }

    /// Sets (`None`: clears) a page's own scale; every measurement of the page is put on the
    /// scale that now applies where it is, in the same undo step.
    pub fn set_page_scale(&mut self, page: u32, scale: Option<ScaleSetting>) -> Result<Outcome> {
        self.apply(Command::SetPageScale {
            page: page as usize,
            scale,
        })
    }

    /// Sets the scale of every page from `first` to `last` (both included) as one undo step.
    pub fn apply_scale_to_pages(
        &mut self,
        first: u32,
        last: u32,
        scale: Option<ScaleSetting>,
    ) -> Result<Outcome> {
        self.apply(Command::ApplyScaleToPages {
            first: first as usize,
            last: last as usize,
            scale,
        })
    }

    /// The scale at which the page distance from `a` to `b` is `real_metres`, stated in
    /// `real_unit` (calibration from two picked points and a typed length). The page's
    /// `/UserUnit` is taken into account.
    pub fn calibrated_scale(
        &self,
        page: u32,
        a: Point,
        b: Point,
        real_metres: f64,
        real_unit: Unit,
    ) -> Result<Scale> {
        let unit = self.page_user_unit(page);
        Scale::calibrate(a, b, real_metres, unit, real_unit)
            .map_err(|e| EditError::Scale(format!("the calibration cannot be used: {e}")).into())
    }

    /// Calibrates a page: its scale becomes the one at which `a` to `b` is `real_metres`
    /// (see [`calibrated_scale`](Document::calibrated_scale)), shown as `format` says (by
    /// default the usual way for `real_unit`).
    pub fn calibrate(
        &mut self,
        page: u32,
        a: Point,
        b: Point,
        real_metres: f64,
        real_unit: Unit,
        format: Option<&MeasureFormat>,
    ) -> Result<Outcome> {
        let scale = self.calibrated_scale(page, a, b, real_metres, real_unit)?;
        self.set_page_scale(page, Some(ScaleSetting::new(&scale, format)))
    }

    /// Adds a viewport: a region of the page with a scale of its own. `bbox` is
    /// `[x0, y0, x1, y1]` in unrotated page space.
    pub fn add_viewport(
        &mut self,
        page: u32,
        name: &str,
        bbox: [f64; 4],
        scale: Option<ScaleSetting>,
    ) -> Result<Outcome> {
        self.apply(Command::AddViewport {
            page: page as usize,
            name: name.to_string(),
            bbox,
            scale,
        })
    }

    pub fn edit_viewport(
        &mut self,
        page: u32,
        name: &str,
        bbox: [f64; 4],
        scale: Option<ScaleSetting>,
    ) -> Result<Outcome> {
        self.apply(Command::EditViewport {
            page: page as usize,
            name: name.to_string(),
            bbox,
            scale,
        })
    }

    pub fn remove_viewport(&mut self, page: u32, name: &str) -> Result<Outcome> {
        self.apply(Command::RemoveViewport {
            page: page as usize,
            name: name.to_string(),
        })
    }

    /// Takes a measurement on the scale that applies where it is, with its caption upright on
    /// the page as displayed. Returns its id.
    pub fn add_measurement(
        &mut self,
        page: u32,
        shape: MeasureShape,
        origin: &Origin,
    ) -> Result<AnnotId> {
        let rotation = self
            .pages()
            .get(page as usize)
            .map_or(0, |g| g.rotate().degrees());
        let a = measure_ops::new_measurement(
            self.annots()?.scales(),
            page as usize,
            shape,
            rotation,
            origin,
        )?;
        let id = a.id.clone();
        self.apply(Command::add(a))?;
        Ok(id)
    }

    /// Gives a measurement new geometry ([`measure_ops::set_geometry`]).
    pub fn set_measure_geometry(
        &mut self,
        id: &AnnotId,
        geometry: Geometry,
        when: PdfDate,
    ) -> Result<Outcome> {
        let ed = self.annots()?;
        let command = measure_ops::set_geometry(ed.set(), ed.scales(), id, geometry, when)?;
        self.apply(command)
    }

    pub fn add_cutout(&mut self, id: &AnnotId, ring: Vec<Point>, when: PdfDate) -> Result<Outcome> {
        let command = measure_ops::add_cutout(self.annots()?.set(), id, ring, when)?;
        self.apply(command)
    }

    pub fn remove_cutout(&mut self, id: &AnnotId, index: usize, when: PdfDate) -> Result<Outcome> {
        let command = measure_ops::remove_cutout(self.annots()?.set(), id, index, when)?;
        self.apply(command)
    }

    /// Sets the depth of a volume (an area given one becomes a volume), in metres.
    pub fn set_depth(&mut self, id: &AnnotId, metres: f64, when: PdfDate) -> Result<Outcome> {
        let command = measure_ops::set_depth(self.annots()?.set(), id, metres, when)?;
        self.apply(command)
    }

    pub fn add_count_marker(&mut self, id: &AnnotId, p: Point, when: PdfDate) -> Result<Outcome> {
        let command = measure_ops::add_count_marker(self.annots()?.set(), id, p, when)?;
        self.apply(command)
    }

    pub fn remove_count_marker(
        &mut self,
        id: &AnnotId,
        index: usize,
        when: PdfDate,
    ) -> Result<Outcome> {
        let command = measure_ops::remove_count_marker(self.annots()?.set(), id, index, when)?;
        self.apply(command)
    }

    /// Every measurement of the document with its value and scale.
    pub fn measurements(&mut self) -> Result<Vec<MeasurementRow>> {
        Ok(measure_ops::measurements(self.annots()?.set()))
    }

    /// Snaps `cursor` (unrotated page space) on `page`: measurement points first, then markup
    /// vertices, then the drawing's endpoints, intersections, midpoints and nearest points
    /// (see [`crate::snap`]). Never waits: the first query of a page starts its content scene
    /// on a worker thread and answers from the markups, or [`SnapOutcome::Pending`]; the
    /// markups count only when the annotations are loaded already.
    pub fn snap(&mut self, page: u32, cursor: Point, options: &SnapOptions) -> SnapOutcome {
        let content = options.content != pl_scene::SnapModes::NONE;
        let scene = if content {
            self.scene(page)
        } else {
            crate::SceneState::Unavailable(pl_scene::NoSnapReason::Cancelled)
        };
        snap::snap(
            self.editor().map(|e| e.set()),
            &scene,
            page as usize,
            cursor,
            options,
        )
    }

    // ---- takeoff definitions -----------------------------------------------------------------

    /// The document settings (default scale, legends, spaces, sequences).
    pub fn doc_settings(&mut self) -> Result<&DocSettings> {
        Ok(self.annots()?.scales().settings())
    }

    /// Replaces the document settings as one undo step.
    pub fn set_doc_settings(&mut self, settings: DocSettings) -> Result<Outcome> {
        self.apply(Command::SetDocSettings(Box::new(settings)))
    }

    fn change_settings(&mut self, change: impl FnOnce(&mut DocSettings)) -> Result<Outcome> {
        let mut settings = self.doc_settings()?.clone();
        change(&mut settings);
        self.set_doc_settings(settings)
    }

    /// Sets the scale of every page that has none of its own.
    pub fn set_default_scale(&mut self, scale: Option<ScaleSetting>) -> Result<Outcome> {
        self.change_settings(|s| s.default_scale = scale)
    }

    /// Adds a legend, or replaces the one with its id.
    pub fn define_legend(&mut self, legend: LegendDef) -> Result<Outcome> {
        self.change_settings(|s| upsert(&mut s.legends, legend, |l| l.id.clone()))
    }

    pub fn remove_legend(&mut self, id: &str) -> Result<Outcome> {
        self.change_settings(|s| s.legends.retain(|l| l.id != id))
    }

    /// Adds a space, or replaces the one with its id.
    pub fn define_space(&mut self, space: SpaceDef) -> Result<Outcome> {
        self.change_settings(|s| upsert(&mut s.spaces, space, |x| x.id.clone()))
    }

    pub fn remove_space(&mut self, id: &str) -> Result<Outcome> {
        self.change_settings(|s| s.spaces.retain(|x| x.id != id))
    }

    /// Adds a sequence, or replaces the one with its id.
    pub fn define_sequence(&mut self, sequence: SequenceDef) -> Result<Outcome> {
        self.change_settings(|s| upsert(&mut s.sequences, sequence, |x| x.id.clone()))
    }

    pub fn remove_sequence(&mut self, id: &str) -> Result<Outcome> {
        self.change_settings(|s| s.sequences.retain(|x| x.id != id))
    }

    /// Hands out the next label of a sequence and moves the sequence on, as one undo step.
    pub fn take_sequence_label(&mut self, id: &str) -> Result<String> {
        let mut settings = self.doc_settings()?.clone();
        let seq = settings
            .sequences
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| EditError::Settings(format!("there is no sequence {id:?}")))?;
        let label = seq.label(seq.next);
        seq.next = seq.next.checked_add(seq.step).ok_or_else(|| {
            EditError::Settings(format!("sequence {id:?} has run out of numbers"))
        })?;
        self.set_doc_settings(settings)?;
        Ok(label)
    }

    /// What changed since this was last called: the refresh hook for legends, spaces and
    /// sequences (see [`TakeoffRefresh`]). Empty when the store is not loaded.
    pub fn take_takeoff_refresh(&mut self) -> TakeoffRefresh {
        match self.edit.state.as_mut() {
            Some(state) => state.editor.take_takeoff_refresh(),
            None => TakeoffRefresh::default(),
        }
    }
}

fn upsert<T>(list: &mut Vec<T>, item: T, key: impl Fn(&T) -> String) {
    let k = key(&item);
    match list.iter_mut().find(|x| key(x) == k) {
        Some(slot) => *slot = item,
        None => list.push(item),
    }
}
