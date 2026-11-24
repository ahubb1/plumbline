//! Shared helpers: the page layouts every script runs on, and a rig that plays inputs given in
//! **screen pixels** (x right, y down, origin at the top-left of the page as displayed).
//!
//! A script says what the user does on screen and what they should then see on screen. The
//! rig turns screen positions into unrotated page space with the page's own transform, the
//! way the application will, so one script checks a tool on every rotation, `/UserUnit` and
//! box offset.
#![allow(dead_code)]

use pl_annot::{AnnotId, Annotation, Geometry, Origin, PdfDate};
use pl_draw::DrawList;
use pl_geom::{PageGeometry, Point, Rect, Rotation, Vec2};
use pl_tools::{
    step, step_text, DocView, Intent, Key, Modifiers, Pointer, SimpleView, TextInput, ToolInput,
    ToolKind, ToolState,
};

pub const NONE: Modifiers = Modifiers::NONE;
pub const SHIFT: Modifiers = Modifiers::SHIFT;
pub const CTRL: Modifiers = Modifiers::CTRL;
pub const ALT: Modifiers = Modifiers::ALT;

pub fn origin() -> Origin {
    Origin::new(
        "Plumbline Tester",
        PdfDate::parse("D:20261124101500Z").unwrap(),
    )
}

/// One page to run a script on.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub name: &'static str,
    pub geometry: PageGeometry,
    /// Page units per screen pixel: 1 at 100% zoom, 0.5 on the `/UserUnit 2` page.
    pub px: f64,
}

impl Layout {
    /// A screen position as a page-space point.
    pub fn at(&self, sx: f64, sy: f64) -> Point {
        self.geometry.display_to_page() * Point::new(sx * self.px, sy * self.px)
    }

    /// The page-space rectangle between two screen corners.
    pub fn rect(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> Rect {
        Rect::from_points(self.at(x0, y0), self.at(x1, y1))
    }

    /// A page-space point as a screen position.
    pub fn screen(&self, p: Point) -> Point {
        let d = self.geometry.page_to_display() * p;
        Point::new(d.x / self.px, d.y / self.px)
    }

    /// A screen offset as a page-space vector.
    pub fn vec(&self, dx: f64, dy: f64) -> Vec2 {
        self.at(dx, dy) - self.at(0.0, 0.0)
    }

    pub fn rotation(&self) -> Rotation {
        self.geometry.rotate()
    }
}

/// The same eight layouts pl-annot and pl-draw test on: plain, `/Rotate` 90, 180 and 270,
/// `/UserUnit 2`, an inset CropBox, a MediaBox with an offset origin, and CropBox plus
/// rotation. Every one shows at least 450 x 450 screen pixels.
pub fn layouts() -> Vec<Layout> {
    let letter = Rect::new(0.0, 0.0, 612.0, 792.0);
    let offset = Rect::new(100.0, 200.0, 712.0, 992.0);
    let page = |name, media, crop, rotate, unit: f64| Layout {
        name,
        geometry: PageGeometry::new(media, crop, rotate, unit),
        px: 1.0 / unit,
    };
    vec![
        page("plain", letter, None, Rotation::R0, 1.0),
        page("rotate90", letter, None, Rotation::R90, 1.0),
        page("rotate180", letter, None, Rotation::R180, 1.0),
        page("rotate270", letter, None, Rotation::R270, 1.0),
        page(
            "userunit",
            Rect::new(0.0, 0.0, 306.0, 396.0),
            None,
            Rotation::R0,
            2.0,
        ),
        page(
            "cropbox",
            letter,
            Some(Rect::new(36.0, 72.0, 540.0, 684.0)),
            Rotation::R0,
            1.0,
        ),
        page("mediabox-offset", offset, None, Rotation::R0, 1.0),
        page(
            "cropbox-rotate90",
            offset,
            Some(Rect::new(150.0, 260.0, 650.0, 900.0)),
            Rotation::R90,
            1.0,
        ),
    ]
}

/// The page index scripts run on. Page 0 is a plain page that nothing should ever land on.
pub const PAGE: usize = 1;

/// A document, a tool, and the last preview.
pub struct Rig {
    pub layout: Layout,
    pub view: SimpleView,
    pub state: ToolState,
    pub preview: DrawList,
}

impl Rig {
    pub fn new(layout: &Layout, tool: ToolKind) -> Rig {
        let mut view = SimpleView::new(
            vec![PageGeometry::simple(612.0, 792.0), layout.geometry],
            origin(),
        );
        view.pixel_size = layout.px;
        Rig {
            layout: *layout,
            view,
            state: ToolState::for_tool(tool),
            preview: DrawList::new(),
        }
    }

    pub fn pointer(&self, sx: f64, sy: f64, modifiers: Modifiers) -> Pointer {
        Pointer::new(PAGE, self.layout.at(sx, sy)).with_modifiers(modifiers)
    }

    /// Plays one input, carries out its intents on the view, and returns them without the
    /// cursor requests.
    pub fn send(&mut self, input: ToolInput) -> Vec<Intent> {
        let (state, intents, preview) = step(self.state.clone(), &input, &self.view);
        preview.validate().expect("a preview is always drawable");
        self.view.apply(&intents);
        self.state = state;
        self.preview = preview;
        intents
            .into_iter()
            .filter(|i| !matches!(i, Intent::SetCursor(_)))
            .collect()
    }

    /// Gives the tool the text editor's answer and carries out its intents.
    pub fn text(&mut self, input: TextInput) -> Vec<Intent> {
        let (state, intents, preview) = step_text(self.state.clone(), &input, &self.view);
        preview.validate().expect("a preview is always drawable");
        self.view.apply(&intents);
        self.state = state;
        self.preview = preview;
        intents
    }

    /// The editor closes with `text`.
    pub fn commit(&mut self, text: &str) -> Vec<Intent> {
        self.text(TextInput::Commit(text.to_string()))
    }

    pub fn down(&mut self, sx: f64, sy: f64, m: Modifiers) -> Vec<Intent> {
        self.send(ToolInput::PointerDown(self.pointer(sx, sy, m)))
    }

    pub fn mv(&mut self, sx: f64, sy: f64, m: Modifiers) -> Vec<Intent> {
        self.send(ToolInput::PointerMove(self.pointer(sx, sy, m)))
    }

    pub fn up(&mut self, sx: f64, sy: f64, m: Modifiers) -> Vec<Intent> {
        self.send(ToolInput::PointerUp(self.pointer(sx, sy, m)))
    }

    pub fn double(&mut self, sx: f64, sy: f64) -> Vec<Intent> {
        self.send(ToolInput::DoubleClick(self.pointer(sx, sy, NONE)))
    }

    pub fn key(&mut self, key: Key, modifiers: Modifiers) -> Vec<Intent> {
        self.send(ToolInput::Key { key, modifiers })
    }

    /// Press at `from`, move through the midpoint to `to`, release there; the modifiers are
    /// held from the first move on. Returns the intents of the whole gesture.
    pub fn drag(&mut self, from: (f64, f64), to: (f64, f64), m: Modifiers) -> Vec<Intent> {
        let mut out = self.down(from.0, from.1, NONE);
        out.extend(self.mv((from.0 + to.0) / 2.0, (from.1 + to.1) / 2.0, m));
        out.extend(self.mv(to.0, to.1, m));
        out.extend(self.up(to.0, to.1, m));
        out
    }

    /// Press and release at one place.
    pub fn click(&mut self, sx: f64, sy: f64, m: Modifiers) -> Vec<Intent> {
        let mut out = self.down(sx, sy, m);
        out.extend(self.up(sx, sy, m));
        out
    }

    /// Adds an annotation to the document directly and returns its id.
    pub fn add(&mut self, a: Annotation) -> AnnotId {
        let id = a.id.clone();
        self.view.annotations.push(a);
        id
    }

    pub fn get(&self, id: &AnnotId) -> &Annotation {
        self.view.get(id).expect("the annotation exists")
    }

    pub fn geometry(&self, id: &AnnotId) -> Geometry {
        self.get(id).geometry.clone()
    }

    pub fn selection(&self) -> Vec<AnnotId> {
        self.view.selection().to_vec()
    }

    /// The annotations on the script page.
    pub fn on_page(&self) -> Vec<&Annotation> {
        self.view.annotations(PAGE)
    }
}

/// The one annotation a gesture created; panics unless the intents are exactly a create,
/// optionally followed by selecting it and opening its text.
pub fn created(intents: &[Intent]) -> Annotation {
    let Some(Intent::Create(a)) = intents.first() else {
        panic!("expected a Create first, got {intents:?}");
    };
    for extra in &intents[1..] {
        match extra {
            Intent::Select(ids) => assert_eq!(ids, std::slice::from_ref(&a.id)),
            Intent::RequestTextEdit(id) => assert_eq!(*id, a.id),
            other => panic!("unexpected intent {other:?}"),
        }
    }
    (**a).clone()
}

/// Asserts two rectangles are the same to within `1e-9`.
#[track_caller]
pub fn assert_rect(got: Rect, want: Rect, what: &str) {
    let d = (got.x0 - want.x0)
        .abs()
        .max((got.y0 - want.y0).abs())
        .max((got.x1 - want.x1).abs())
        .max((got.y1 - want.y1).abs());
    assert!(d <= 1e-9, "{what}: got {got:?}, want {want:?}");
}

/// Asserts two points are the same to within `1e-9`.
#[track_caller]
pub fn assert_point(got: Point, want: Point, what: &str) {
    assert!(
        (got - want).hypot() <= 1e-9,
        "{what}: got {got:?}, want {want:?}"
    );
}

/// The rectangle of an annotation whose geometry is one.
#[track_caller]
pub fn rect_of(a: &Annotation) -> Rect {
    match a.geometry {
        Geometry::Rect(r) => r,
        ref other => panic!("not a rectangle: {other:?}"),
    }
}

/// A rectangle as the user sees it: left, top, right, bottom in screen pixels.
pub fn on_screen(l: &Layout, r: Rect) -> Rect {
    Rect::from_points(
        l.screen(Point::new(r.x0, r.y0)),
        l.screen(Point::new(r.x1, r.y1)),
    )
}

pub fn line(a: Point, b: Point) -> Geometry {
    Geometry::Line { a, b }
}
