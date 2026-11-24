//! Random command sequences against the [`Editor`]: every state the history has been through
//! is reached again exactly by undo and redo, and every command's stated inverse does what
//! undo does. No renderer is involved; half the runs start from annotations read out of a
//! real PDF (so they carry their tie to the file), half from an empty document.
//!
//! The commands include text boxes, callouts, typewriter text, label stamps and picture
//! stamps, and everything the markups list does through `pl_doc::markup_ops`: text, review
//! status, replies and their deletion, checkmarks, locks, custom column values and popups.
//! Each of those is also put through JSON, the journal's form, before it is run.
//!
//! And the measurement commands: page scales (one page and ranges), viewports added, edited
//! and removed, the document's default scale and spaces, new measurements of every kind on
//! the scale that applies where they are, and their geometry, cutouts, depth and count markers.
//! A state includes every page's scales and the document settings.

use std::sync::{Arc, OnceLock};

use pl_doc::markup_ops;
use pl_doc::measure_ops::{self, MeasureShape};
use pl_doc::pl_annot::{
    AnnotId, AnnotKind, AnnotSet, Annotation, CountSymbol, FontStyle, Geometry, HAlign, LineEnding,
    MeasureKind, Origin, PdfDate, Quad, ReviewState, Rgb, RgbaImage, StampSpec, VAlign,
};
use pl_doc::pl_geom::{Point, Rect, Vec2};
use pl_doc::pl_pdf::PdfDocument;
use pl_doc::pl_takeoff::{ColumnType, Schema, Value};
use pl_doc::{Command, DocSettings, Editor, PageScalesData, ScaleSetting, ScaleStore, SpaceDef};

/// xorshift64*: small, seeded and the same on every machine.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn coord(&mut self) -> f64 {
        // Quarter units: exact in single precision, as a saved file would hold them.
        self.below(2000) as f64 / 4.0
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

fn origin() -> Origin {
    Origin::new("Random", PdfDate::from_unix(1_800_000_000))
}

/// One small picture shared by every picture stamp, as an imported file would be.
fn picture() -> Arc<RgbaImage> {
    static PICTURE: OnceLock<Arc<RgbaImage>> = OnceLock::new();
    PICTURE
        .get_or_init(|| {
            let pixels: Vec<u8> = (0..4 * 3 * 4u32).map(|i| (i * 37 % 256) as u8).collect();
            Arc::new(RgbaImage::new(4, 3, pixels).unwrap())
        })
        .clone()
}

/// The custom columns of the random runs.
fn schema() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        let mut s = Schema::new();
        s.add("Trade", ColumnType::Text, &[]).unwrap();
        s.add("Cost", ColumnType::Number, &[]).unwrap();
        s.add("Phase", ColumnType::Choice, &["Enabling", "Main works"])
            .unwrap();
        s.add("Done", ColumnType::Checkmark, &[]).unwrap();
        s
    })
}

const WORDS: [&str; 6] = [
    "Check the falls to the gully.",
    "Kerb here",
    "REV C",
    "Line one\nline two\nand a third, rather longer, line that has to wrap",
    "x",
    "Fire rated door set - see schedule",
];

fn random_annotation(rng: &mut Rng, pages: usize) -> Annotation {
    let o = origin();
    let page = rng.below(pages);
    let p = |rng: &mut Rng| Point::new(rng.coord(), rng.coord());
    let rect = |rng: &mut Rng| {
        let a = p(rng);
        Rect::new(a.x, a.y, a.x + 5.0 + rng.coord(), a.y + 5.0 + rng.coord())
    };
    let mut a = match rng.below(14) {
        9 => {
            let mut a = Annotation::free_text(page, rect(rng), WORDS[rng.below(6)], &o);
            if let Some(t) = a.free_text.as_mut() {
                t.font = FontStyle::ALL[rng.below(4)];
                t.size = 6.0 + rng.below(12) as f64;
                t.align = [HAlign::Left, HAlign::Centre, HAlign::Right][rng.below(3)];
                t.v_align = [VAlign::Top, VAlign::Middle, VAlign::Bottom][rng.below(3)];
            }
            a.set_rotation(90 * rng.below(4) as i32);
            a
        }
        10 => {
            let r = rect(rng);
            let mut a = Annotation::callout(page, r, p(rng), WORDS[rng.below(6)], &o);
            a.set_rotation(90 * rng.below(4) as i32);
            a
        }
        11 => Annotation::typewriter(page, rect(rng), WORDS[rng.below(6)], &o),
        12 => {
            let set = StampSpec::standard_set();
            let mut spec = set[rng.below(set.len())].clone();
            if rng.chance(50) {
                spec = spec.with_detail("Random, 24 Nov 2026");
            }
            let mut a = Annotation::stamp(page, rect(rng), spec, &o);
            a.set_rotation(90 * rng.below(4) as i32);
            a
        }
        13 => Annotation::image_stamp(page, rect(rng), picture(), &o),
        0 => Annotation::square(page, rect(rng), &o),
        1 => Annotation::circle(page, rect(rng), &o),
        2 => Annotation::line(page, p(rng), p(rng), &o),
        3 => Annotation::polyline(page, (0..3 + rng.below(4)).map(|_| p(rng)).collect(), &o),
        4 => Annotation::polygon(page, (0..3 + rng.below(4)).map(|_| p(rng)).collect(), &o),
        5 => Annotation::ink(
            page,
            (0..1 + rng.below(3))
                .map(|_| (0..2 + rng.below(6)).map(|_| p(rng)).collect())
                .collect(),
            &o,
        ),
        6 => {
            let r = rect(rng);
            let quad: Quad = [
                Point::new(r.x0, r.y0),
                Point::new(r.x1, r.y0),
                Point::new(r.x1, r.y1),
                Point::new(r.x0, r.y1),
            ];
            Annotation::text_markup(AnnotKind::Highlight, page, vec![quad], &o)
        }
        7 => Annotation::note(page, rect(rng), "a note", &o),
        _ => Annotation::square(page, rect(rng), &o),
    };
    if rng.chance(40) {
        a.style.fill = Some(Rgb::new(0.25, 0.5, 0.75));
    }
    if rng.chance(30) {
        a.style.width = 1.0 + rng.below(6) as f64;
    }
    if rng.chance(20) {
        a.style.dash = Some(vec![3.0, 2.0]);
    }
    a
}

fn random_change(rng: &mut Rng, a: &Annotation) -> Annotation {
    let mut b = a.clone();
    if b.free_text.is_some() && rng.chance(50) {
        // A change only a text box has.
        let turn = 90 * rng.below(4) as i32;
        match rng.below(5) {
            0 => b.free_text.as_mut().unwrap().size = 5.0 + rng.below(20) as f64,
            1 => b.free_text.as_mut().unwrap().color = Rgb::new(0.0, 0.25, 0.5),
            2 => {
                b.free_text.as_mut().unwrap().align =
                    [HAlign::Left, HAlign::Centre, HAlign::Right][rng.below(3)]
            }
            3 => b.set_rotation(turn),
            // Sometimes text it cannot show, to exercise the refusal.
            _ => {
                b.meta.contents = Some(if rng.chance(30) {
                    "not WinAnsi: \u{4e2d}\u{6587}".to_string()
                } else {
                    WORDS[rng.below(6)].to_string()
                })
            }
        }
        return b;
    }
    match rng.below(6) {
        0 | 1 => b.translate(Vec2::new(rng.coord() - 250.0, rng.coord() - 250.0)),
        2 => b.style.opacity = [0.25, 0.5, 0.75, 1.0][rng.below(4)],
        3 => b.meta.contents = Some(format!("text {}", rng.below(1000))),
        4 => {
            b.style.stroke = Some(Rgb::new(0.0, 0.5, 1.0));
            b.style.line_end = LineEnding::ClosedArrow;
        }
        _ => b.meta.subject = Some("Changed".into()),
    }
    b
}

fn pick(rng: &mut Rng, ed: &Editor) -> Option<Annotation> {
    let n = ed.set().len();
    (n > 0).then(|| ed.set().iter().nth(rng.below(n)).unwrap().clone())
}

/// Something the markups list does to a random annotation, as the command
/// `pl_doc::markup_ops` builds for it. `None` when the builder refuses (a status for a reply,
/// a popup for a link, ...) or there is nothing to act on.
fn list_command(rng: &mut Rng, ed: &Editor) -> Option<Command> {
    let a = pick(rng, ed)?;
    let set = ed.set();
    let when = PdfDate::from_unix(1_800_000_000 + rng.below(100_000) as i64);
    let who = Origin::new(["Ada", "Bo", "Cy"][rng.below(3)], when);
    let page_box = Rect::new(0.0, 0.0, 612.0, 792.0);
    let columns = &schema().columns;
    let command = match rng.below(13) {
        0 => markup_ops::set_text(set, &a.id, WORDS[rng.below(6)], when),
        // Text a text box cannot show: built here, refused (for a text box) when executed.
        1 => markup_ops::set_text(set, &a.id, "\u{2192} \u{4e2d}", when),
        2 => {
            markup_ops::set_status(set, &a.id, ReviewState::ALL[rng.below(5)], &who).map(|(c, _)| c)
        }
        3 | 4 => markup_ops::add_reply(set, &a.id, WORDS[rng.below(6)], &who).map(|(c, _)| c),
        5 => {
            // A reply or status, if there is one, else whatever was picked (refused).
            let target = set
                .iter()
                .filter(|r| r.meta.reply_to.is_some())
                .nth(rng.below(4))
                .map_or(a.id.clone(), |r| r.id.clone());
            markup_ops::delete_reply(set, &target)
        }
        6 => markup_ops::toggle_checked(set, &a.id, &who),
        7 => markup_ops::toggle_locked(set, &a.id, when),
        8 => {
            let column = &columns[rng.below(columns.len())];
            let value = match column.kind {
                ColumnType::Number => Value::number(rng.below(4000) as f64 / 8.0),
                ColumnType::Checkmark => Value::Bool(rng.chance(50)),
                ColumnType::Choice => Value::text(["Enabling", "main works"][rng.below(2)]),
                _ => Value::text(WORDS[rng.below(6)]),
            };
            markup_ops::set_column(set, &a.id, column, value, when)
        }
        9 => {
            let column = &columns[rng.below(columns.len())];
            markup_ops::clear_column(set, &a.id, &column.id, when)
        }
        10 => markup_ops::ensure_popup(set, &a.id, page_box, &who)
            .map(|(c, _)| c.unwrap_or(Command::Batch(Vec::new()))),
        11 => markup_ops::set_popup_open(set, &a.id, rng.chance(50), when),
        _ => {
            let at = Point::new(rng.coord(), rng.coord());
            let rect = Rect::new(at.x, at.y, at.x + 180.0, at.y + 120.0);
            markup_ops::set_popup_rect(set, &a.id, rect, when)
        }
    };
    let command = command.ok()?;
    // The journal's form gives the same command back.
    let json = serde_json::to_string(&command).unwrap();
    let back: Command = serde_json::from_str(&json).unwrap();
    assert_eq!(back, command, "{json}");
    Some(command)
}

const SCALES: [&str; 5] = ["1\" = 20'", "1:100", "1/4\" = 1'-0\"", "1\" = 5'", "1:50"];

fn random_setting(rng: &mut Rng) -> Option<ScaleSetting> {
    (!rng.chance(15)).then(|| ScaleSetting::parse(SCALES[rng.below(SCALES.len())], None).unwrap())
}

/// A scale, viewport or settings command; some of them refused (an unknown viewport, a
/// duplicate name, a box with no area, a backwards page range).
fn scale_command(rng: &mut Rng, ed: &Editor) -> Command {
    let pages = ed.page_count();
    let page = rng.below(pages);
    let name = ["A", "B", "C"][rng.below(3)].to_string();
    let bbox = |rng: &mut Rng| {
        let (x, y) = (rng.coord(), rng.coord());
        let (w, h) = if rng.chance(3) {
            (0.0, 10.0)
        } else {
            (20.0 + rng.coord(), 20.0 + rng.coord())
        };
        [x, y, x + w, y + h]
    };
    let command = match rng.below(9) {
        0 | 1 => Command::SetPageScale {
            page,
            scale: random_setting(rng),
        },
        2 => {
            let (a, b) = (rng.below(pages), rng.below(pages));
            Command::ApplyScaleToPages {
                first: a,
                last: b,
                scale: random_setting(rng),
            }
        }
        3 | 4 => Command::AddViewport {
            page,
            name,
            bbox: bbox(rng),
            scale: random_setting(rng),
        },
        5 => Command::EditViewport {
            page,
            name,
            bbox: bbox(rng),
            scale: random_setting(rng),
        },
        6 => Command::RemoveViewport { page, name },
        7 => {
            let mut settings = ed.scales().settings().clone();
            settings.default_scale = random_setting(rng);
            Command::SetDocSettings(Box::new(settings))
        }
        _ => {
            let mut settings: DocSettings = ed.scales().settings().clone();
            let id = format!("s{}", rng.below(3));
            settings.spaces.retain(|s| s.id != id);
            if rng.chance(70) {
                let (x, y) = (rng.coord(), rng.coord());
                settings.spaces.push(SpaceDef {
                    id,
                    name: "Room".into(),
                    page,
                    outline: vec![[x, y], [x + 50.0, y], [x + 50.0, y + 40.0]],
                    parent: None,
                });
            }
            Command::SetDocSettings(Box::new(settings))
        }
    };
    let json = serde_json::to_string(&command).unwrap();
    let back: Command = serde_json::from_str(&json).unwrap();
    assert_eq!(back, command, "{json}");
    command
}

/// A new measurement, or a change to one, as `pl_doc::measure_ops` builds it. `None` when the
/// builder refuses (no scale where it is, a cutout outside the outline, ...).
fn measure_command(rng: &mut Rng, ed: &Editor) -> Option<Command> {
    let o = origin();
    let when = PdfDate::from_unix(1_800_000_000 + rng.below(100_000) as i64);
    let p = |rng: &mut Rng| Point::new(rng.coord(), rng.coord());
    let measured: Vec<Annotation> = ed
        .set()
        .iter()
        .filter(|a| a.measurement().is_some())
        .cloned()
        .collect();
    if measured.is_empty() || rng.chance(45) {
        let page = rng.below(ed.page_count());
        let (x, y) = (rng.coord(), rng.coord());
        let (w, h) = (10.0 + rng.coord(), 10.0 + rng.coord());
        let outline = vec![
            Point::new(x, y),
            Point::new(x + w, y),
            Point::new(x + w, y + h),
            Point::new(x, y + h),
        ];
        let hole = vec![
            Point::new(x + 1.0, y + 1.0),
            Point::new(x + w / 2.0, y + 1.0),
            Point::new(x + w / 2.0, y + h / 2.0),
        ];
        let shape = match rng.below(9) {
            0 => MeasureShape::Length {
                a: p(rng),
                b: p(rng),
            },
            1 => MeasureShape::Polylength((0..2 + rng.below(3)).map(|_| p(rng)).collect()),
            2 => MeasureShape::Perimeter(outline),
            3 => MeasureShape::Area {
                outline,
                cutouts: if rng.chance(50) {
                    vec![hole]
                } else {
                    Vec::new()
                },
            },
            4 => MeasureShape::Volume {
                outline,
                cutouts: Vec::new(),
                depth: 0.25 * (1 + rng.below(8)) as f64,
            },
            5 => MeasureShape::Angle {
                a: p(rng),
                vertex: p(rng),
                b: p(rng),
            },
            6 => MeasureShape::Radius {
                centre: p(rng),
                edge: p(rng),
            },
            7 => MeasureShape::Diameter {
                a: p(rng),
                b: p(rng),
            },
            _ => MeasureShape::Count {
                points: (0..rng.below(4)).map(|_| p(rng)).collect(),
                symbol: CountSymbol::ALL[rng.below(CountSymbol::ALL.len())],
            },
        };
        let rotation = 90 * rng.below(4) as i32;
        let a = measure_ops::new_measurement(ed.scales(), page, shape, rotation, &o).ok()?;
        return Some(Command::add(a));
    }
    let a = &measured[rng.below(measured.len())];
    let kind = a.measurement().unwrap().kind;
    let set = ed.set();
    let command = match rng.below(5) {
        0 => {
            let geometry = match &a.geometry {
                Geometry::Line { a: s, b: e } => Geometry::Line {
                    a: *s,
                    b: *e + Vec2::new(rng.coord() / 10.0, 1.0),
                },
                Geometry::Points(points) => {
                    let mut points = points.clone();
                    let i = rng.below(points.len());
                    points[i] += Vec2::new(rng.coord() / 10.0, -2.0);
                    Geometry::Points(points)
                }
                other => other.clone(),
            };
            measure_ops::set_geometry(set, ed.scales(), &a.id, geometry, when)
        }
        1 if matches!(kind, MeasureKind::Area | MeasureKind::Volume) => {
            let Geometry::Points(outline) = &a.geometry else {
                return None;
            };
            // A small triangle at the first vertex, which may or may not be inside.
            let c = outline[0];
            let ring = vec![
                c + Vec2::new(0.5, 0.5),
                c + Vec2::new(2.0, 0.5),
                c + Vec2::new(0.5, 2.0),
            ];
            measure_ops::add_cutout(set, &a.id, ring, when)
        }
        2 if matches!(kind, MeasureKind::Area | MeasureKind::Volume) => {
            match a.measurement().unwrap().cutouts.len() {
                0 => measure_ops::set_depth(set, &a.id, 0.5 + rng.below(4) as f64, when),
                n => measure_ops::remove_cutout(set, &a.id, rng.below(n), when),
            }
        }
        3 if kind == MeasureKind::Count => measure_ops::add_count_marker(set, &a.id, p(rng), when),
        4 if kind == MeasureKind::Count => {
            let n = a.count_points().len();
            measure_ops::remove_count_marker(set, &a.id, rng.below(n.max(1)), when)
        }
        _ => measure_ops::set_depth(set, &a.id, 1.0, when),
    };
    let command = command.ok()?;
    let json = serde_json::to_string(&command).unwrap();
    let back: Command = serde_json::from_str(&json).unwrap();
    assert_eq!(back, command, "{json}");
    Some(command)
}

/// A random command that may or may not be acceptable.
fn random_command(rng: &mut Rng, ed: &Editor, depth: usize) -> Command {
    let pages = ed.page_count();
    if rng.chance(22) {
        return if rng.chance(40) {
            scale_command(rng, ed)
        } else {
            measure_command(rng, ed).unwrap_or_else(|| scale_command(rng, ed))
        };
    }
    match rng.below(if depth == 0 { 17 } else { 13 }) {
        10..=12 => match list_command(rng, ed) {
            Some(command) => command,
            None => Command::add(random_annotation(rng, pages)),
        },
        13 | 14 if depth == 0 => match list_command(rng, ed) {
            Some(command) => command,
            None => Command::add(random_annotation(rng, pages)),
        },
        0..=3 => Command::AddAnnotations(
            (0..1 + rng.below(3))
                .map(|_| random_annotation(rng, pages))
                .collect(),
        ),
        4..=6 => match pick(rng, ed) {
            Some(a) => {
                let mut items = vec![random_change(rng, &a)];
                if rng.chance(30) {
                    if let Some(other) = pick(rng, ed).filter(|o| o.id != a.id) {
                        items.push(random_change(rng, &other));
                    }
                }
                Command::ModifyAnnotations(items)
            }
            None => Command::add(random_annotation(rng, pages)),
        },
        7 | 8 => match pick(rng, ed) {
            Some(a) => {
                let mut ids = vec![a.id.clone()];
                if rng.chance(30) {
                    ids.extend(pick(rng, ed).map(|o| o.id).filter(|id| *id != a.id));
                }
                if rng.chance(3) {
                    // Sometimes something that cannot work, to exercise the rollback.
                    ids.push(AnnotId::from("no-such-annotation"));
                }
                Command::DeleteAnnotations(ids)
            }
            None => Command::add(random_annotation(rng, pages)),
        },
        9 => {
            let page = rng.below(pages);
            let mut order: Vec<AnnotId> = ed.set().on_page(page).map(|a| a.id.clone()).collect();
            for i in (1..order.len()).rev() {
                order.swap(i, rng.below(i + 1));
            }
            order.truncate(1 + rng.below(order.len().max(1)));
            Command::Reorder { page, order }
        }
        _ => Command::Batch(
            (0..1 + rng.below(4))
                .map(|_| random_command(rng, ed, depth + 1))
                .collect(),
        ),
    }
}

type State = (
    Vec<Annotation>,
    Vec<Annotation>,
    Vec<PageScalesData>,
    DocSettings,
);

/// Everything a state consists of: the annotations in order with their ties to the file, the
/// file annotations awaiting deletion (their order is of no consequence to a save), every
/// page's scales and the document settings.
fn snapshot(ed: &Editor) -> State {
    let items: Vec<Annotation> = ed.set().iter().cloned().collect();
    let mut removed: Vec<Annotation> = ed.set().removed().to_vec();
    removed.sort_by(|a, b| a.id.cmp(&b.id));
    (
        items,
        removed,
        ed.scales().data(),
        ed.scales().settings().clone(),
    )
}

fn start_sets() -> Vec<(AnnotSet, ScaleStore, usize)> {
    let mut sets = vec![(AnnotSet::new(), ScaleStore::empty(3), 3)];
    for fixture in [
        pl_testkit::annotations(),
        pl_testkit::rotated_pages(),
        pl_testkit::user_unit(),
    ] {
        let doc = PdfDocument::from_bytes(fixture.bytes.clone(), None).unwrap();
        sets.push((
            AnnotSet::load(&doc),
            ScaleStore::load(&doc),
            doc.page_count(),
        ));
    }
    assert!(
        sets[1].0.len() >= 4,
        "the annotations fixture has foreign markups"
    );
    sets
}

#[test]
fn a_thousand_random_sequences_undo_to_the_start_and_redo_to_the_end() {
    let starts = start_sets();
    let mut commands_run = 0usize;
    let mut refused = 0usize;
    let mut merged = 0usize;
    let mut scale_steps = 0usize;
    let mut measure_steps = 0usize;
    for seed in 0..1000u64 {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ (seed + 1).wrapping_mul(0xff51_afd7_ed55_8ccd));
        let (set, scales, pages) = starts[seed as usize % starts.len()].clone();
        let mut ed = Editor::with_scales(set, scales, pages);
        let start = snapshot(&ed);
        let was_dirty = ed.set().is_dirty();
        assert!(!ed.scales().is_dirty());
        // timeline[k] is the state with k undo steps beneath it.
        let mut timeline = vec![start.clone()];

        let steps = 5 + rng.below(40);
        for _ in 0..steps {
            match rng.below(10) {
                0 | 1 if ed.can_undo() => {
                    // The stated inverse, run on a copy, gives the model that undo gives.
                    let inverse = ed.inverse_of_last().unwrap();
                    let mut copy = ed.clone();
                    ed.undo().unwrap();
                    assert_eq!(snapshot(&ed), timeline[ed.undo_len()], "seed {seed}: undo");
                    copy.execute(inverse.clone())
                        .unwrap_or_else(|e| panic!("seed {seed}: inverse {inverse:?}: {e}"));
                    assert_eq!(
                        copy.model(),
                        ed.model(),
                        "seed {seed}: inverse as a command"
                    );
                    assert_eq!(
                        (copy.scales().data(), copy.scales().settings()),
                        (ed.scales().data(), ed.scales().settings()),
                        "seed {seed}: inverse as a command, scales"
                    );
                    assert_eq!(
                        copy.has_unsaved(),
                        ed.has_unsaved(),
                        "seed {seed}: the inverse leaves the same to save"
                    );
                }
                2 if ed.can_redo() => {
                    ed.redo().unwrap();
                    assert_eq!(snapshot(&ed), timeline[ed.undo_len()], "seed {seed}: redo");
                }
                3 => {
                    // A drag: several steps on one annotation under one key.
                    let Some(a) = pick(&mut rng, &ed) else {
                        continue;
                    };
                    if a.measurement().is_some()
                        && measure_ops::geometry_points(&a.geometry).is_empty()
                    {
                        // A count with no markers yet has nothing to drag.
                        continue;
                    }
                    let key = rng.next();
                    let before = ed.undo_len();
                    let mut current = a;
                    let mut any = false;
                    for _ in 0..2 + rng.below(5) {
                        let mut next = current.clone();
                        next.translate(Vec2::new(0.25 + rng.coord(), 0.5));
                        let out = ed
                            .execute_merged(Command::modify(next.clone()), key)
                            .unwrap();
                        assert!(out.changed);
                        merged += usize::from(out.merged);
                        any = true;
                        current = next;
                    }
                    assert!(any);
                    assert_eq!(ed.undo_len(), before + 1, "seed {seed}: a drag is one step");
                    timeline.truncate(before + 1);
                    timeline.push(snapshot(&ed));
                }
                _ => {
                    let command = random_command(&mut rng, &ed, 0);
                    let before = snapshot(&ed);
                    let depth = ed.undo_len();
                    match ed.execute(command.clone()) {
                        Ok(out) if out.changed => {
                            commands_run += 1;
                            assert_eq!(ed.undo_len(), depth + 1);
                            let after = snapshot(&ed);
                            let scales_changed = after.2 != before.2 || after.3 != before.3;
                            assert!(!out.touched.is_empty() || scales_changed);
                            scale_steps += usize::from(scales_changed);
                            measure_steps += usize::from(out.touched.iter().any(|id| {
                                ed.set().get(id).is_some_and(|a| a.measurement().is_some())
                            }));
                            // A measurement's text is always its value.
                            for a in ed.set().iter().filter(|a| a.measurement().is_some()) {
                                assert_eq!(a.meta.contents, a.measure_value_text(), "seed {seed}");
                            }
                            timeline.truncate(depth + 1);
                            timeline.push(after);
                        }
                        Ok(_) => {
                            assert_eq!(snapshot(&ed), before, "seed {seed}: a no-op changed");
                            assert_eq!(ed.undo_len(), depth);
                        }
                        Err(_) => {
                            refused += 1;
                            assert_eq!(
                                snapshot(&ed),
                                before,
                                "seed {seed}: a refused command changed something: {command:?}"
                            );
                            assert_eq!(ed.undo_len(), depth);
                        }
                    }
                }
            }
            // The set stays in page order, which is the order a reload gives.
            let pages: Vec<usize> = ed.set().iter().map(|a| a.page).collect();
            assert!(pages.is_sorted(), "seed {seed}: page order");
        }

        // Back to the very start ...
        while ed.can_redo() {
            ed.redo().unwrap();
        }
        let end = snapshot(&ed);
        let depth = ed.undo_len();
        assert_eq!(end, timeline[depth], "seed {seed}");
        while ed.can_undo() {
            ed.undo().unwrap();
            assert_eq!(
                snapshot(&ed),
                timeline[ed.undo_len()],
                "seed {seed}: undo all"
            );
        }
        assert_eq!(snapshot(&ed), start, "seed {seed}: the start state");
        assert!(!ed.is_modified(), "seed {seed}");
        assert_eq!(
            ed.set().is_dirty(),
            was_dirty,
            "seed {seed}: nothing is left for a save to write"
        );
        assert!(
            !ed.scales().is_dirty(),
            "seed {seed}: no scale is left for a save to write"
        );
        // ... and forward again to the very end.
        while ed.can_redo() {
            ed.redo().unwrap();
            assert_eq!(
                snapshot(&ed),
                timeline[ed.undo_len()],
                "seed {seed}: redo all"
            );
        }
        assert_eq!(ed.undo_len(), depth);
        assert_eq!(snapshot(&ed), end, "seed {seed}: the end state");
    }
    // The generator really did exercise all the paths.
    println!(
        "{commands_run} commands run, {refused} refused, {merged} merged drag steps, \
         {scale_steps} scale changes, {measure_steps} measurement changes"
    );
    assert!(scale_steps > 500, "{scale_steps} scale changes");
    assert!(measure_steps > 500, "{measure_steps} measurement changes");
    assert!(commands_run > 8000, "{commands_run} commands");
    assert!(refused > 50, "{refused} refused");
    assert!(merged > 1000, "{merged} merged drag steps");
}
