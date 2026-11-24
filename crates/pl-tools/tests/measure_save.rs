//! Measurements made by the tools, saved through pl-annot and pl-pdf with the page's scale
//! (`/VP`) on every fixture layout, and read back with the same values. The view here is a
//! `DocView` over an `AnnotSet` and a `PageViewports` loaded from the file, the way pl-doc
//! will build one.
//!
//! The real-world sample (`samples/site-development-plans.pdf`) is only read: a sheet is
//! calibrated and measured and the save is planned in memory, never written. Without the file
//! the test prints a line and passes.

mod common;

use std::path::{Path, PathBuf};

use common::{origin, Layout, NONE};
use pl_annot::{
    AnnotId, AnnotSet, Annotation, Geometry, MeasureFormat, MeasureKind, MeasureSpec, Origin,
    PageViewports, ScaleRef,
};
use pl_geom::{PageGeometry, Scale, Unit};
use pl_pdf::{PdfDocument, PdfFile, SaveOptions};
use pl_tools::{
    step, step_measure, DocView, Intent, Key, MeasureInput, Modifiers, Pointer, ScaleTarget,
    ToolInput, ToolKind, ToolPreset, ToolState,
};

/// A `DocView` over an annotation set and one page's scales.
struct SetView {
    pages: Vec<PageGeometry>,
    set: AnnotSet,
    vps: PageViewports,
    selection: Vec<AnnotId>,
    px: f64,
}

impl SetView {
    fn new(doc: &PdfDocument, page: usize, px: f64) -> SetView {
        SetView {
            pages: doc.pages().unwrap().iter().map(|p| p.geometry).collect(),
            set: AnnotSet::load(doc),
            vps: PageViewports::load(doc, page).unwrap(),
            selection: Vec::new(),
            px,
        }
    }

    fn apply(&mut self, intents: &[Intent]) {
        for intent in intents {
            match intent {
                Intent::Create(a) => {
                    self.set.add((**a).clone()).unwrap();
                }
                Intent::Modify { id, change } => {
                    let a = self.set.get_mut(id).unwrap();
                    assert!(change.apply(a));
                    a.touch(origin().when);
                }
                Intent::Select(ids) => self.selection = ids.clone(),
                Intent::SetScale(change) => {
                    assert_eq!(change.target, ScaleTarget::Page);
                    self.vps.page_scale = Some(change.spec.clone());
                }
                Intent::Delete(_) | Intent::Reorder { .. } => panic!("not in this script"),
                _ => {}
            }
        }
    }
}

impl DocView for SetView {
    fn page(&self, page: usize) -> Option<PageGeometry> {
        self.pages.get(page).copied()
    }
    fn annotations(&self, page: usize) -> Vec<&Annotation> {
        self.set.on_page(page).collect()
    }
    fn annotation(&self, id: &AnnotId) -> Option<&Annotation> {
        self.set.get(id)
    }
    fn selection(&self) -> &[AnnotId] {
        &self.selection
    }
    fn pixel_size(&self) -> f64 {
        self.px
    }
    fn origin(&self) -> Origin {
        origin()
    }
    fn viewports(&self, page: usize) -> Option<&PageViewports> {
        (page == self.vps.page).then_some(&self.vps)
    }
}

struct Player {
    view: SetView,
    state: ToolState,
    page: usize,
    layout: Layout,
}

impl Player {
    fn new(doc: &PdfDocument, page: usize) -> Player {
        let geometry = doc.page(page).unwrap().geometry;
        let px = 1.0 / geometry.user_unit();
        Player {
            view: SetView::new(doc, page, px),
            state: ToolState::for_tool(ToolKind::Select),
            page,
            layout: Layout {
                name: "file",
                geometry,
                px,
            },
        }
    }

    fn tool(&mut self, tool: ToolKind) {
        self.state = ToolState::for_tool(tool);
    }

    fn send(&mut self, input: ToolInput) -> Vec<Intent> {
        let (state, intents, preview) = step(self.state.clone(), &input, &self.view);
        preview.validate().unwrap();
        self.view.apply(&intents);
        self.state = state;
        intents
    }

    fn answer(&mut self, input: MeasureInput) {
        let (state, intents, _) = step_measure(self.state.clone(), &input, &self.view);
        self.view.apply(&intents);
        self.state = state;
    }

    fn at(&self, x: f64, y: f64, m: Modifiers) -> Pointer {
        Pointer::new(self.page, self.layout.at(x, y)).with_modifiers(m)
    }

    fn click(&mut self, x: f64, y: f64) {
        self.send(ToolInput::PointerDown(self.at(x, y, NONE)));
        self.send(ToolInput::PointerUp(self.at(x, y, NONE)));
    }

    fn drag(&mut self, from: (f64, f64), to: (f64, f64)) {
        self.send(ToolInput::PointerDown(self.at(from.0, from.1, NONE)));
        self.send(ToolInput::PointerMove(self.at(to.0, to.1, NONE)));
        self.send(ToolInput::PointerUp(self.at(to.0, to.1, NONE)));
    }

    /// Calibrates two inches of paper as twenty feet, then takes one measurement of each kind
    /// inside the top-left 440 x 440 screen pixels.
    fn measure_one_of_each(&mut self) {
        self.tool(ToolKind::Calibrate);
        self.click(20.0, 420.0);
        self.click(164.0, 420.0);
        self.answer(MeasureInput::Distance {
            metres: 20.0 * 0.3048,
            unit: Unit::Foot,
        });
        self.tool(ToolKind::Length);
        self.drag((40.0, 40.0), (112.0, 40.0));
        self.tool(ToolKind::Polylength);
        self.click(40.0, 80.0);
        self.click(112.0, 80.0);
        self.click(112.0, 152.0);
        self.send(ToolInput::key(Key::Enter));
        self.tool(ToolKind::Area);
        self.drag((160.0, 40.0), (304.0, 184.0));
        self.tool(ToolKind::Cutout);
        self.click(232.0, 112.0);
        self.drag((196.0, 76.0), (268.0, 148.0));
        let mut volume = ToolPreset::default_for(ToolKind::Volume);
        volume.measure.as_mut().unwrap().depth = Some(0.3048);
        self.state = ToolState::new(volume);
        self.drag((320.0, 40.0), (392.0, 112.0));
        self.tool(ToolKind::Angle);
        self.click(112.0, 220.0);
        self.click(40.0, 220.0);
        self.click(40.0, 184.0);
        self.tool(ToolKind::Radius);
        self.click(200.0, 260.0);
        self.click(236.0, 260.0);
        self.tool(ToolKind::Diameter);
        self.click(320.0, 260.0);
        self.click(356.0, 260.0);
        self.tool(ToolKind::Count);
        for x in [40.0, 80.0, 120.0] {
            self.click(x, 360.0);
        }
        self.send(ToolInput::key(Key::Enter));
    }
}

const KINDS: [MeasureKind; 8] = [
    MeasureKind::Length,
    MeasureKind::Polylength,
    MeasureKind::Area,
    MeasureKind::Volume,
    MeasureKind::Angle,
    MeasureKind::Radius,
    MeasureKind::Diameter,
    MeasureKind::Count,
];

/// What every layout must read: the same values whatever its rotation, boxes or user unit.
const VALUES: [&str; 8] = [
    "10.00'",
    "20.00'",
    "300.00 sq ft",
    "3.70 cu yd",
    "90.0\u{b0}",
    // A radius and a diameter: the bare value, as Revu writes /Contents.
    "5.00'",
    "10.00'",
    "3",
];

fn measured(view: &SetView, page: usize, had: usize) -> Vec<Annotation> {
    view.set
        .on_page(page)
        .skip(had)
        .map(Annotation::detached)
        .collect()
}

#[test]
fn measurements_save_with_their_scale_and_read_back_the_same_on_every_layout() {
    type Target = (&'static str, fn() -> pl_testkit::Fixture, usize);
    let targets: [Target; 8] = [
        ("plain", pl_testkit::plain, 0),
        ("rotate90", pl_testkit::rotated_pages, 1),
        ("rotate180", pl_testkit::rotated_pages, 2),
        ("rotate270", pl_testkit::rotated_pages, 3),
        ("userunit", pl_testkit::user_unit, 0),
        ("cropbox", pl_testkit::crop_box, 0),
        ("mediabox-offset", pl_testkit::crop_box, 1),
        ("cropbox-rotate90", pl_testkit::crop_box, 2),
    ];
    for (name, fixture, page) in targets {
        let fixture = fixture();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(fixture.manifest.file_name());
        std::fs::write(&path, &fixture.bytes).unwrap();
        let mut file = PdfFile::open(&path).unwrap();
        let doc = file.document().unwrap().clone();
        let had = AnnotSet::load(&doc).on_page(page).count();

        let mut player = Player::new(&doc, page);
        let uu = player.layout.geometry.user_unit();
        player.measure_one_of_each();
        let drawn = measured(&player.view, page, had);
        let kinds: Vec<MeasureKind> = drawn
            .iter()
            .map(|a| a.measurement().expect("a measurement").kind)
            .collect();
        assert_eq!(kinds, KINDS, "{name}");
        let values: Vec<&str> = drawn
            .iter()
            .map(|a| a.meta.contents.as_deref().unwrap())
            .collect();
        assert_eq!(values, VALUES, "{name}");
        let scale = Scale::engineering(10.0).unwrap();
        let spec = MeasureSpec::new(&scale, uu, &MeasureFormat::for_scale(&scale));
        assert_eq!(
            player.view.vps.page_scale.as_ref().unwrap().ratio,
            spec.ratio
        );
        for a in &drawn {
            assert_eq!(a.measurement().unwrap().scale, ScaleRef::Page, "{name}");
        }

        // Save the markups and the page's scale together, re-open, compare.
        let mut plan = player.view.set.plan_save(&doc).expect("plan");
        assert_eq!(plan.added.len(), KINDS.len(), "{name}");
        assert!(player.view.vps.plan(&doc, &mut plan.edits).unwrap());
        file.save_incremental(&plan.edits, &SaveOptions::default())
            .expect("verified incremental save");
        let saved = file.document().unwrap().clone();
        assert!(saved.bytes().starts_with(&fixture.bytes), "{name}: prefix");
        let back = Player::new(&saved, page);
        assert!(
            !back.view.set.is_dirty() && !back.view.vps.is_dirty(),
            "{name}"
        );
        let reread = back.view.vps.page_scale.as_ref().expect("the page scale");
        assert_eq!(reread.ratio, "1 in = 10 ft", "{name}");
        assert!(
            (reread.x_factor() / spec.x_factor() - 1.0).abs() < 1e-9,
            "{name}"
        );
        let again = measured(&back.view, page, had);
        assert_eq!(again.len(), drawn.len());
        for (a, b) in drawn.iter().zip(&again) {
            assert_eq!(a.id, b.id);
            assert_eq!(b.meta.contents, a.meta.contents, "{name}: {:?}", a.kind);
            assert_eq!(b.measure_text(), a.measure_text(), "{name}");
            let (qa, qb) = (a.quantities().unwrap(), b.quantities().unwrap());
            for (x, y) in [
                (qa.length, qb.length),
                (qa.area, qb.area),
                (qa.volume, qb.volume),
                (qa.angle, qb.angle),
            ] {
                match (x, y) {
                    (Some(x), Some(y)) => {
                        assert!((x - y).abs() <= 1e-6 * x.abs().max(1.0), "{name}: {x} {y}")
                    }
                    (x, y) => assert_eq!(x, y, "{name}"),
                }
            }
            assert_eq!(
                a.measurement().unwrap().cutouts.len(),
                b.measurement().unwrap().cutouts.len()
            );
        }

        // A vertex of the reloaded polylength is dragged with the Select tool and the value
        // follows it into the file.
        let mut player = back;
        player.view.selection = vec![drawn[1].id.clone()];
        player.tool(ToolKind::Select);
        player.drag((112.0, 152.0), (112.0, 224.0));
        let plan = player.view.set.plan_save(&saved).expect("plan");
        assert_eq!(plan.modified.len(), 1, "{name}");
        file.save_incremental(&plan.edits, &SaveOptions::default())
            .expect("verified incremental save");
        let last = file.document().unwrap().clone();
        let reloaded = AnnotSet::load(&last);
        let p = reloaded.get(&drawn[1].id).unwrap();
        assert_eq!(p.meta.contents.as_deref(), Some("30.00'"), "{name}");
        let Geometry::Points(points) = &p.geometry else {
            panic!("a polyline");
        };
        assert_eq!(points.len(), 3);
    }
}

fn sample_path() -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("samples")
        .join("site-development-plans.pdf");
    path.is_file().then_some(path)
}

#[test]
fn measuring_a_sheet_of_the_real_sample() {
    let Some(path) = sample_path() else {
        eprintln!("SAMPLE SKIPPED: samples/site-development-plans.pdf is not present");
        return;
    };
    let stamp = |p: &Path| {
        std::fs::metadata(p)
            .map(|m| (m.len(), m.modified().ok()))
            .unwrap()
    };
    let before = stamp(&path);
    let mut file = PdfFile::open(&path).unwrap();
    let doc = file.document().unwrap().clone();
    let page = 0;
    let had = AnnotSet::load(&doc).on_page(page).count();
    let mut player = Player::new(&doc, page);
    let l = player.layout;
    let size = l.geometry.display_size();
    assert!(size.width / l.px >= 440.0 && size.height / l.px >= 440.0);
    player.measure_one_of_each();
    let drawn = measured(&player.view, page, had);
    let values: Vec<&str> = drawn
        .iter()
        .map(|a| a.meta.contents.as_deref().unwrap())
        .collect();
    // The same values as on the fixtures: a measurement depends on the scale and the paper,
    // not on the sheet's size, rotation or boxes.
    assert_eq!(values, VALUES);
    let crop = l.geometry.crop_box();
    for a in &drawn {
        let b = a.bounds();
        assert!(b.x0 >= crop.x0 && b.x1 <= crop.x1 && b.y0 >= crop.y0 && b.y1 <= crop.y1);
    }
    // The save is planned, not written.
    let mut plan = player
        .view
        .set
        .plan_save(&doc)
        .expect("a plan for the real file");
    assert_eq!(plan.added.len(), KINDS.len());
    assert!(player.view.vps.plan(&doc, &mut plan.edits).unwrap());
    drop(file);
    assert_eq!(stamp(&path), before, "the sample is untouched");
}
