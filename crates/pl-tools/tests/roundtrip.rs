//! Tools against real files: every drawing tool is played on a page of each pl-testkit
//! fixture layout, its markups are saved through pl-annot and pl-pdf and read back equal, and
//! the Select tool then edits the loaded markups. The view here is a `DocView` over a
//! pl-annot `AnnotSet`, the way pl-doc will build one.
//!
//! The real-world sample (`samples/site-development-plans.pdf`) is only read: tools are played
//! on its sheets and the save is planned in memory, never written. Without the file the test
//! prints a line and passes.

mod common;

use std::path::{Path, PathBuf};

use common::{origin, Layout, CTRL, NONE, SHIFT};
use pl_annot::{AnnotId, AnnotKind, AnnotSet, Annotation, Geometry, Origin};
use pl_geom::PageGeometry;
use pl_pdf::{PdfDocument, PdfFile, SaveOptions};
use pl_tools::{step, DocView, Intent, Key, Modifiers, Pointer, ToolInput, ToolKind, ToolState};

/// A `DocView` over an annotation set and the document's own page geometry.
struct SetView {
    pages: Vec<PageGeometry>,
    set: AnnotSet,
    selection: Vec<AnnotId>,
    px: f64,
}

impl SetView {
    fn new(doc: &PdfDocument, px: f64) -> SetView {
        SetView {
            pages: doc.pages().unwrap().iter().map(|p| p.geometry).collect(),
            set: AnnotSet::load(doc),
            selection: Vec::new(),
            px,
        }
    }

    /// Carries out intents the way pl-doc's commands will.
    fn apply(&mut self, intents: &[Intent]) {
        for intent in intents {
            match intent {
                Intent::Create(a) => {
                    self.set
                        .add((**a).clone())
                        .expect("a tool's markup can be added");
                }
                Intent::Modify { id, change } => {
                    let a = self.set.get_mut(id).expect("the modified markup exists");
                    assert!(change.apply(a), "the change fits the markup");
                    a.touch(origin().when);
                }
                Intent::Delete(ids) => {
                    for id in ids {
                        assert!(!self.set.remove(id).is_empty());
                    }
                    self.selection.retain(|id| !ids.contains(id));
                }
                Intent::Select(ids) => self.selection = ids.clone(),
                Intent::Reorder { page, order } => self.set.set_order(*page, order),
                Intent::SetCursor(_)
                | Intent::RequestTextEdit(_)
                | Intent::SetScale(_)
                | Intent::RequestMeasure(_) => {}
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
}

/// Plays screen-pixel scripts on one page of a `SetView`.
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
            view: SetView::new(doc, px),
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

    fn at(&self, x: f64, y: f64, m: Modifiers) -> Pointer {
        Pointer::new(self.page, self.layout.at(x, y)).with_modifiers(m)
    }

    fn drag(&mut self, from: (f64, f64), to: (f64, f64), m: Modifiers) {
        self.send(ToolInput::PointerDown(self.at(from.0, from.1, NONE)));
        self.send(ToolInput::PointerMove(self.at(
            (from.0 + to.0) / 2.0,
            (from.1 + to.1) / 2.0,
            m,
        )));
        self.send(ToolInput::PointerUp(self.at(to.0, to.1, m)));
    }

    fn click(&mut self, x: f64, y: f64, m: Modifiers) {
        self.send(ToolInput::PointerDown(self.at(x, y, m)));
        self.send(ToolInput::PointerUp(self.at(x, y, m)));
    }

    /// One markup from every drawing tool, inside the top-left 440 x 440 screen pixels.
    fn draw_one_of_each(&mut self) {
        self.tool(ToolKind::Rectangle);
        self.drag((40.0, 40.0), (140.0, 100.0), NONE);
        self.tool(ToolKind::Ellipse);
        self.drag((160.0, 40.0), (260.0, 90.0), SHIFT);
        self.tool(ToolKind::Line);
        self.drag((40.0, 160.0), (140.0, 170.0), SHIFT);
        self.tool(ToolKind::Arrow);
        self.drag((160.0, 160.0), (250.0, 220.0), NONE);
        for (tool, dx) in [
            (ToolKind::Polyline, 0.0),
            (ToolKind::Polygon, 130.0),
            (ToolKind::Cloud, 260.0),
        ] {
            self.tool(tool);
            self.click(40.0 + dx, 260.0, NONE);
            self.click(140.0 + dx, 260.0, NONE);
            self.click(140.0 + dx, 330.0, NONE);
            self.click(60.0 + dx, 340.0, NONE);
            self.send(ToolInput::DoubleClick(self.at(60.0 + dx, 340.0, NONE)));
        }
        self.tool(ToolKind::Pen);
        self.send(ToolInput::PointerDown(self.at(300.0, 40.0, NONE)));
        for i in 1..=40 {
            let t = i as f64 / 40.0 * std::f64::consts::PI;
            self.send(ToolInput::PointerMove(self.at(
                300.0 + i as f64 * 2.0,
                40.0 + 30.0 * t.sin(),
                NONE,
            )));
        }
        self.send(ToolInput::PointerUp(self.at(380.0, 40.0, NONE)));
        self.tool(ToolKind::Highlighter);
        self.drag((300.0, 120.0), (400.0, 123.0), SHIFT);
        self.tool(ToolKind::Note);
        self.click(330.0, 190.0, NONE);
    }
}

/// The two geometries have the same shape and every coordinate within `tolerance`.
fn assert_close(a: &Geometry, b: &Geometry, tolerance: f64, name: &str) {
    let flat = |g: &Geometry| match g {
        Geometry::Ink(strokes) => strokes.clone(),
        other => panic!("{name}: not ink: {other:?}"),
    };
    let (a, b) = (flat(a), flat(b));
    assert_eq!(a.len(), b.len(), "{name}");
    for (s, t) in a.iter().zip(&b) {
        assert_eq!(s.len(), t.len(), "{name}");
        for (p, q) in s.iter().zip(t) {
            assert!(
                (*p - *q).hypot() <= tolerance,
                "{name}: {p:?} against {q:?}"
            );
        }
    }
}

fn kinds_on(view: &SetView, page: usize) -> Vec<AnnotKind> {
    view.set.on_page(page).map(|a| a.kind.clone()).collect()
}

const DRAWN: [AnnotKind; 10] = [
    AnnotKind::Square,
    AnnotKind::Circle,
    AnnotKind::Line,
    AnnotKind::Line,
    AnnotKind::PolyLine,
    AnnotKind::Polygon,
    AnnotKind::Polygon,
    AnnotKind::Ink,
    AnnotKind::Ink,
    AnnotKind::Text,
];

#[test]
fn tool_output_saves_and_reloads_equal_on_every_fixture_layout() {
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
        let had = AnnotSet::load(&doc).len();

        let mut player = Player::new(&doc, page);
        let l = player.layout;
        player.draw_one_of_each();
        assert_eq!(kinds_on(&player.view, page)[had..], DRAWN, "{name}");
        let drawn: Vec<Annotation> = player
            .view
            .set
            .on_page(page)
            .skip(had)
            .map(Annotation::detached)
            .collect();
        // Golden geometry, in screen terms, for the constrained ones.
        assert_eq!(
            drawn[1].geometry,
            Geometry::Rect(l.rect(160.0, 40.0, 260.0, 140.0)),
            "{name}"
        );
        assert_eq!(
            drawn[2].geometry,
            Geometry::Line {
                a: l.at(40.0, 160.0),
                b: l.at(140.0, 160.0)
            },
            "{name}"
        );
        assert_eq!(
            drawn[8].geometry,
            Geometry::Ink(vec![vec![l.at(300.0, 120.0), l.at(400.0, 120.0)]]),
            "{name}"
        );
        // Everything a tool draws lies on the visible page.
        let crop = l.geometry.crop_box();
        for a in &drawn {
            let b = a.bounds();
            assert!(
                b.x0 >= crop.x0 && b.x1 <= crop.x1 && b.y0 >= crop.y0 && b.y1 <= crop.y1,
                "{name}: {:?} at {b:?} is inside {crop:?}",
                a.kind
            );
        }

        // Save, re-open, compare.
        let plan = player.view.set.plan_save(&doc).expect("plan");
        assert_eq!(plan.added.len(), DRAWN.len(), "{name}");
        file.save_incremental(&plan.edits, &SaveOptions::default())
            .expect("verified incremental save");
        let saved = file.document().unwrap().clone();
        assert!(
            saved.bytes().starts_with(&fixture.bytes),
            "{name}: exact prefix"
        );
        let mut player = Player::new(&saved, page);
        assert!(!player.view.set.is_dirty());
        for (i, a) in drawn.iter().enumerate() {
            let mut back = player.view.set.get(&a.id).expect("it was saved").detached();
            if i == 7 {
                // The freehand curve has coordinates that are not round numbers; the file
                // holds them to about seven significant digits (see pl-annot).
                assert_close(&back.geometry, &a.geometry, 1e-3, name);
                back.geometry = a.geometry.clone();
            }
            assert_eq!(back, *a, "{name}: {:?} reads back as drawn", a.kind);
        }

        // Now edit the loaded markups with the Select tool: move the rectangle, pull the
        // circle's bottom-right screen handle keeping its proportions, drag the line's end,
        // leave a copy of the arrow, nudge and restack the note, delete the polyline.
        let id = |i: usize| drawn[i].id.clone();
        player.tool(ToolKind::Select);
        player.drag((40.0, 70.0), (60.0, 80.0), NONE);
        player.click(210.0, 40.0, NONE);
        assert_eq!(
            player.view.selection,
            [id(1)],
            "{name}: the circle's top is picked"
        );
        player.drag((260.0, 140.0), (280.0, 150.0), SHIFT);
        player.click(90.0, 160.0, NONE);
        player.drag((140.0, 160.0), (150.0, 200.0), NONE);
        player.drag((205.0, 190.0), (205.0, 240.0), CTRL);
        let copy = player.view.selection.clone();
        assert_eq!(copy.len(), 1);
        assert!(
            !drawn.iter().any(|a| a.id == copy[0]),
            "{name}: a new markup"
        );
        player.click(330.0, 190.0, NONE);
        assert_eq!(player.view.selection, [id(9)]);
        player.send(ToolInput::Key {
            key: Key::Down,
            modifiers: SHIFT,
        });
        let restack = pl_tools::reorder_intent(&player.view, page, pl_tools::send_to_back)
            .expect("the note was on top");
        player.view.apply(&[restack]);
        player.click(90.0, 260.0, NONE);
        assert_eq!(player.view.selection, [id(4)]);
        player.send(ToolInput::key(Key::Delete));

        let expect = |view: &SetView| {
            let g = |i: usize| view.set.get(&id(i)).unwrap().geometry.clone();
            assert_eq!(
                g(0),
                Geometry::Rect(l.rect(60.0, 50.0, 160.0, 110.0)),
                "{name}"
            );
            assert_eq!(
                g(1),
                Geometry::Rect(l.rect(160.0, 40.0, 280.0, 160.0)),
                "{name}"
            );
            assert_eq!(
                g(2),
                Geometry::Line {
                    a: l.at(40.0, 160.0),
                    b: l.at(150.0, 200.0)
                },
                "{name}"
            );
            assert_eq!(g(3), drawn[3].geometry, "{name}: the arrow stayed");
            assert_eq!(
                view.set.get(&copy[0]).unwrap().geometry,
                Geometry::Line {
                    a: l.at(160.0, 210.0),
                    b: l.at(250.0, 270.0)
                },
                "{name}"
            );
            // Ten page units down the screen is 10 / px pixels.
            assert_eq!(
                g(9),
                Geometry::Rect(l.rect(321.0, 181.0 + 10.0 / l.px, 339.0, 199.0 + 10.0 / l.px)),
                "{name}"
            );
            assert!(
                view.set.get(&id(4)).is_none(),
                "{name}: the polyline is gone"
            );
            assert_eq!(
                view.set.on_page(page).nth(had).unwrap().id,
                id(9),
                "{name}: restacked"
            );
        };
        expect(&player.view);
        let plan = player.view.set.plan_save(&saved).expect("plan");
        assert_eq!(plan.added.len(), 1, "{name}");
        assert_eq!(plan.modified.len(), 4, "{name}");
        assert_eq!(plan.deleted.len(), 1, "{name}");
        file.save_incremental(&plan.edits, &SaveOptions::default())
            .expect("verified incremental save");
        let again = file.document().unwrap().clone();
        let reloaded = SetView::new(&again, l.px);
        expect(&reloaded);
        assert_eq!(reloaded.set.on_page(page).count(), had + DRAWN.len());
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
fn tools_on_the_sheets_of_the_real_sample() {
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
    assert!(doc.page_count() >= 2);
    let mut total = 0;
    let mut set = AnnotSet::load(&doc);
    let had = set.len();
    for page in [0, doc.page_count() - 1] {
        let mut player = Player::new(&doc, page);
        let l = player.layout;
        let size = l.geometry.display_size();
        assert!(
            size.width / l.px >= 440.0 && size.height / l.px >= 440.0,
            "the sheet is big enough for the script"
        );
        player.draw_one_of_each();
        let drawn: Vec<Annotation> = player
            .view
            .set
            .on_page(page)
            .filter(|a| a.baseline().is_none())
            .map(Annotation::detached)
            .collect();
        assert_eq!(
            drawn.iter().map(|a| a.kind.clone()).collect::<Vec<_>>(),
            DRAWN,
            "page {page}"
        );
        // The same goldens as on the fixtures, whatever this sheet's rotation and boxes.
        assert_eq!(
            drawn[0].geometry,
            Geometry::Rect(l.rect(40.0, 40.0, 140.0, 100.0))
        );
        assert_eq!(
            drawn[1].geometry,
            Geometry::Rect(l.rect(160.0, 40.0, 260.0, 140.0))
        );
        assert_eq!(
            drawn[2].geometry,
            Geometry::Line {
                a: l.at(40.0, 160.0),
                b: l.at(140.0, 160.0)
            }
        );
        let crop = l.geometry.crop_box();
        for a in drawn {
            let b = a.bounds();
            assert!(b.x0 >= crop.x0 && b.x1 <= crop.x1 && b.y0 >= crop.y0 && b.y1 <= crop.y1);
            set.add(a).unwrap();
            total += 1;
        }
    }
    // The save is planned, not written.
    let plan = set.plan_save(&doc).expect("a plan for the real file");
    assert_eq!(plan.added.len(), total);
    assert_eq!(set.len(), had + total);
    assert_eq!(plan.touched_pages(), [0, doc.page_count() - 1]);
    drop(file);
    assert_eq!(stamp(&path), before, "the sample is untouched");
}
