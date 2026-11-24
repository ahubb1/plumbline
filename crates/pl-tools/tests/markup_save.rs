//! The text and placement tools against real files: a text box, typewriter text, a callout, a
//! cloud callout, a cloud box, a stamp, a picture and a highlight are made with the tools on a
//! page of each pl-testkit fixture layout (rotated, `/UserUnit`, offset boxes), saved through
//! pl-annot and pl-pdf, and read back equal.
//!
//! The real-world sample (`samples/site-development-plans.pdf`) is only read: the same script
//! is played on two of its sheets and the save is planned in memory, never written. Without
//! the file the test prints a line and passes.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{origin, Layout, NONE};
use pl_annot::{AnnotKind, AnnotSet, Annotation, Geometry, Quad, RgbaImage, StampContent};
use pl_pdf::{PdfDocument, PdfFile, SaveOptions};
use pl_tools::{
    step, step_text, DocView, Intent, Pointer, SimpleView, TextInput, ToolInput, ToolKind,
    ToolState,
};

/// Plays screen-pixel scripts on one page of a document's page list.
struct Player {
    l: Layout,
    page: usize,
    view: SimpleView,
    state: ToolState,
}

impl Player {
    fn new(doc: &PdfDocument, page: usize) -> Player {
        let pages: Vec<_> = doc.pages().unwrap().iter().map(|p| p.geometry).collect();
        let geometry = pages[page];
        let px = 1.0 / geometry.user_unit();
        let mut view = SimpleView::new(pages, origin());
        view.pixel_size = px;
        Player {
            l: Layout {
                name: "file",
                geometry,
                px,
            },
            page,
            view,
            state: ToolState::for_tool(ToolKind::Select),
        }
    }

    fn finish(&mut self, out: (ToolState, Vec<Intent>, pl_draw::DrawList)) {
        let (state, intents, preview) = out;
        preview.validate().unwrap();
        self.view.apply(&intents);
        self.state = state;
    }

    fn send(&mut self, input: ToolInput) {
        let out = step(self.state.clone(), &input, &self.view);
        self.finish(out);
    }

    fn at(&self, x: f64, y: f64) -> Pointer {
        Pointer::new(self.page, self.l.at(x, y)).with_modifiers(NONE)
    }

    fn drag(&mut self, from: (f64, f64), to: (f64, f64)) {
        self.send(ToolInput::PointerDown(self.at(from.0, from.1)));
        self.send(ToolInput::PointerMove(
            self.at((from.0 + to.0) / 2.0, (from.1 + to.1) / 2.0),
        ));
        self.send(ToolInput::PointerUp(self.at(to.0, to.1)));
    }

    fn click(&mut self, x: f64, y: f64) {
        self.send(ToolInput::PointerDown(self.at(x, y)));
        self.send(ToolInput::PointerUp(self.at(x, y)));
    }

    fn commit(&mut self, text: &str) {
        assert!(self.state.text_draft().is_some(), "the tool waits for text");
        let out = step_text(
            self.state.clone(),
            &TextInput::Commit(text.to_string()),
            &self.view,
        );
        self.finish(out);
        assert!(!self.state.is_busy());
    }

    /// One markup from each text and placement tool, inside the top-left 440 x 440 screen
    /// pixels.
    fn make_one_of_each(&mut self) {
        self.state = ToolState::for_tool(ToolKind::TextBox);
        self.drag((40.0, 40.0), (200.0, 100.0));
        self.commit("Text box\nsecond line");
        self.state = ToolState::for_tool(ToolKind::Typewriter);
        self.click(40.0, 120.0);
        self.commit("Typed note");
        self.state = ToolState::for_tool(ToolKind::Callout);
        self.click(60.0, 300.0);
        self.click(100.0, 260.0);
        self.drag((150.0, 180.0), (270.0, 230.0));
        self.commit("Callout");
        self.state = ToolState::for_tool(ToolKind::CloudCallout);
        self.drag((300.0, 40.0), (400.0, 120.0));
        self.drag((300.0, 160.0), (420.0, 200.0));
        self.commit("Cloud");
        self.state = ToolState::for_tool(ToolKind::CloudBox);
        self.drag((40.0, 340.0), (140.0, 420.0));
        self.state = ToolState::for_tool(ToolKind::Stamp);
        self.click(330.0, 300.0);
        self.state = ToolState::for_tool(ToolKind::Image).with_image(picture());
        self.click(330.0, 380.0);
        self.state = ToolState::for_tool(ToolKind::TextHighlight);
        let l = self.l;
        let quad = |x0: f64, y0: f64, x1: f64, y1: f64| -> Quad {
            [l.at(x0, y1), l.at(x1, y1), l.at(x1, y0), l.at(x0, y0)]
        };
        self.view.text = (0..8)
            .map(|i| {
                let x = 180.0 + 10.0 * f64::from(i);
                (self.page, quad(x, 350.0, x + 10.0, 362.0))
            })
            .collect();
        self.drag((183.0, 356.0), (255.0, 357.0));
    }
}

fn picture() -> Arc<RgbaImage> {
    let pixels = (0..16u32 * 8)
        .flat_map(|i| {
            [
                (i * 2) as u8,
                (255 - i) as u8,
                (i % 16 * 16) as u8,
                if i % 4 == 0 { 128 } else { 255 },
            ]
        })
        .collect();
    Arc::new(RgbaImage::new(16, 8, pixels).unwrap())
}

const MADE: [AnnotKind; 9] = [
    AnnotKind::FreeText,
    AnnotKind::FreeText,
    AnnotKind::FreeText,
    AnnotKind::Square,
    AnnotKind::FreeText,
    AnnotKind::Square,
    AnnotKind::Stamp,
    AnnotKind::Stamp,
    AnnotKind::Highlight,
];

/// What the script made, checked the same way on every page it is played on.
fn check(made: &[Annotation], l: &Layout, name: &str) {
    assert_eq!(
        made.iter().map(|a| a.kind.clone()).collect::<Vec<_>>(),
        MADE,
        "{name}"
    );
    assert_eq!(
        made[0].geometry,
        Geometry::Rect(l.rect(40.0, 40.0, 200.0, 100.0)),
        "{name}"
    );
    assert_eq!(
        made[2].geometry,
        Geometry::Rect(l.rect(150.0, 180.0, 270.0, 230.0)),
        "{name}"
    );
    let leader = &made[2].free_text.as_ref().unwrap().callout;
    assert_eq!(
        leader[..2],
        [l.at(60.0, 300.0), l.at(100.0, 260.0)],
        "{name}"
    );
    assert_eq!(
        made[5].geometry,
        Geometry::Rect(l.rect(40.0, 340.0, 140.0, 420.0)),
        "{name}"
    );
    assert!(matches!(
        made[7].stamp.as_ref().unwrap().content,
        StampContent::Image(_)
    ));
    let crop = l.geometry.crop_box();
    let turn = l.rotation().degrees();
    for a in made {
        assert!(a.is_modelled(), "{name}: {:?}", a.kind);
        a.validate().unwrap();
        assert!(!a.draw_list().is_empty(), "{name}: {:?} paints", a.kind);
        let b = a.bounds();
        assert!(
            b.x0 >= crop.x0 && b.x1 <= crop.x1 && b.y0 >= crop.y0 && b.y1 <= crop.y1,
            "{name}: {:?} at {b:?} is inside {crop:?}",
            a.kind
        );
        if a.free_text.is_some() || a.stamp.is_some() {
            assert_eq!(
                i32::from(a.rotation()),
                turn,
                "{name}: upright as displayed"
            );
        }
    }
}

#[test]
fn text_and_placement_tools_save_and_reload_equal_on_every_fixture_layout() {
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

        let mut player = Player::new(&doc, page);
        player.make_one_of_each();
        let made: Vec<Annotation> = player
            .view
            .annotations(page)
            .into_iter()
            .map(Annotation::detached)
            .collect();
        check(&made, &player.l, name);

        let mut set = AnnotSet::load(&doc);
        let had = set.len();
        for a in &made {
            set.add(a.clone()).expect("a tool's markup can be added");
        }
        let plan = set.plan_save(&doc).expect("plan");
        assert_eq!(plan.added.len(), MADE.len(), "{name}");
        file.save_incremental(&plan.edits, &SaveOptions::default())
            .expect("verified incremental save");
        let saved = file.document().unwrap().clone();
        assert!(
            saved.bytes().starts_with(&fixture.bytes),
            "{name}: exact prefix"
        );
        let back = AnnotSet::load(&saved);
        assert_eq!(back.len(), had + MADE.len(), "{name}");
        assert!(!back.is_dirty(), "{name}");
        for a in &made {
            let read = back.get(&a.id).expect("it was saved").detached();
            assert_eq!(read, *a, "{name}: {:?} reads back as made", a.kind);
        }
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
fn text_and_placement_tools_on_the_sheets_of_the_real_sample() {
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
    let mut set = AnnotSet::load(&doc);
    let had = set.len();
    let mut total = 0;
    for page in [0, doc.page_count() - 1] {
        let mut player = Player::new(&doc, page);
        let size = player.l.geometry.display_size();
        assert!(
            size.width / player.l.px >= 440.0 && size.height / player.l.px >= 440.0,
            "the sheet is big enough for the script"
        );
        player.make_one_of_each();
        let made: Vec<Annotation> = player
            .view
            .annotations(page)
            .into_iter()
            .map(Annotation::detached)
            .collect();
        check(&made, &player.l, &format!("sample page {page}"));
        for a in made {
            set.add(a).unwrap();
            total += 1;
        }
    }
    // The save is planned, not written.
    let plan = set.plan_save(&doc).expect("a plan for the real file");
    assert_eq!(plan.added.len(), total);
    assert_eq!(set.len(), had + total);
    drop(file);
    assert_eq!(stamp(&path), before, "the sample is untouched");
}
