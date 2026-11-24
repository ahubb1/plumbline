//! Align, distribute and stacking order. Positions are screen pixels (see `common`): "left"
//! and "top" mean what the user sees, on every page rotation.

mod common;

use common::*;
use pl_annot::{AnnotId, Annotation, Flags, Geometry};
use pl_tools::{
    align, bring_forward, bring_to_front, distribute, nudge, reorder_intent, send_backward,
    send_to_back, Align, AnnotChange, Axis, DocView, Intent, ToolKind,
};

/// A screen-space description of a markup: a box, a line or a triangle.
#[derive(Clone, Copy)]
enum Shape {
    Box(f64, f64, f64, f64),
    Line(f64, f64, f64, f64),
    /// Base from (x0, y1) to (x1, y1), apex above the middle at y0.
    Tri(f64, f64, f64, f64),
}

impl Shape {
    fn geometry(self, l: &Layout) -> Geometry {
        match self {
            Shape::Box(x0, y0, x1, y1) => Geometry::Rect(l.rect(x0, y0, x1, y1)),
            Shape::Line(x0, y0, x1, y1) => line(l.at(x0, y0), l.at(x1, y1)),
            Shape::Tri(x0, y0, x1, y1) => {
                Geometry::Points(vec![l.at(x0, y1), l.at(x1, y1), l.at((x0 + x1) / 2.0, y0)])
            }
        }
    }

    fn make(self, l: &Layout) -> Annotation {
        let o = origin();
        match self.geometry(l) {
            Geometry::Rect(r) => Annotation::square(PAGE, r, &o),
            Geometry::Line { a, b } => Annotation::line(PAGE, a, b, &o),
            Geometry::Points(p) => Annotation::polygon(PAGE, p, &o),
            _ => unreachable!(),
        }
    }

    fn shifted(self, dx: f64, dy: f64) -> Shape {
        match self {
            Shape::Box(a, b, c, d) => Shape::Box(a + dx, b + dy, c + dx, d + dy),
            Shape::Line(a, b, c, d) => Shape::Line(a + dx, b + dy, c + dx, d + dy),
            Shape::Tri(a, b, c, d) => Shape::Tri(a + dx, b + dy, c + dx, d + dy),
        }
    }
}

/// Screen boxes: A x 100..140, y 100..130; B x 200..260, y 150..210; C x 300..380, y 120..160.
const A: Shape = Shape::Box(100.0, 100.0, 140.0, 130.0);
const B: Shape = Shape::Line(200.0, 150.0, 260.0, 210.0);
const C: Shape = Shape::Tri(300.0, 120.0, 380.0, 160.0);

fn modify(a: &Annotation, geometry: Geometry) -> Intent {
    Intent::Modify {
        id: a.id.clone(),
        change: AnnotChange::Geometry(geometry),
    }
}

/// What `shapes` moved by the given screen offsets come to, as the intents expected for them
/// (none for a zero offset).
fn expect(l: &Layout, items: &[Annotation], shapes: &[Shape], by: &[(f64, f64)]) -> Vec<Intent> {
    items
        .iter()
        .zip(shapes)
        .zip(by)
        .filter(|(_, by)| **by != (0.0, 0.0))
        .map(|((a, s), (dx, dy))| modify(a, s.shifted(*dx, *dy).geometry(l)))
        .collect()
}

#[test]
fn align_lines_up_edges_and_centres_as_seen_on_screen() {
    for l in layouts() {
        let shapes = [A, B, C];
        let items: Vec<Annotation> = shapes.iter().map(|s| s.make(&l)).collect();
        let refs: Vec<&Annotation> = items.iter().collect();
        let r = l.rotation();
        let cases = [
            // Leftmost edge is A's, at 100.
            (Align::Left, [(0.0, 0.0), (-100.0, 0.0), (-200.0, 0.0)]),
            // Rightmost edge is C's, at 380.
            (Align::Right, [(240.0, 0.0), (120.0, 0.0), (0.0, 0.0)]),
            // Topmost edge is A's, at 100.
            (Align::Top, [(0.0, 0.0), (0.0, -50.0), (0.0, -20.0)]),
            // Bottommost edge is B's, at 210.
            (Align::Bottom, [(0.0, 80.0), (0.0, 0.0), (0.0, 50.0)]),
            // Halfway between 100 and 380 is 240; centres are at 120, 230 and 340.
            (Align::Centre, [(120.0, 0.0), (10.0, 0.0), (-100.0, 0.0)]),
            // Halfway between 100 and 210 is 155; middles are at 115, 180 and 140.
            (Align::Middle, [(0.0, 40.0), (0.0, -25.0), (0.0, 15.0)]),
        ];
        for (how, by) in cases {
            let got = align(&refs, how, r);
            assert_eq!(got, expect(&l, &items, &shapes, &by), "{} {how:?}", l.name);
            // Done once, there is nothing left to do.
            let mut view = pl_tools::SimpleView::new(vec![], origin());
            view.annotations = items.clone();
            view.apply(&got);
            let after: Vec<&Annotation> = view.annotations.iter().collect();
            assert_eq!(align(&after, how, r), [], "{} {how:?} twice", l.name);
        }
        // One markup has nothing to line up with.
        assert_eq!(align(&refs[..1], Align::Left, r), []);
        assert_eq!(align(&[], Align::Left, r), []);
    }
}

#[test]
fn a_locked_markup_sets_the_line_but_stays_put() {
    for l in layouts() {
        let shapes = [A, B, C];
        let mut items: Vec<Annotation> = shapes.iter().map(|s| s.make(&l)).collect();
        items[0].meta.flags = items[0].meta.flags.with(Flags::LOCKED, true);
        let refs: Vec<&Annotation> = items.iter().collect();
        // The locked A still gives the left edge.
        assert_eq!(
            align(&refs, Align::Left, l.rotation()),
            expect(
                &l,
                &items,
                &shapes,
                &[(0.0, 0.0), (-100.0, 0.0), (-200.0, 0.0)]
            ),
            "{}",
            l.name
        );
        // To the right, A would have to move and does not.
        assert_eq!(
            align(&refs, Align::Right, l.rotation()),
            expect(&l, &items, &shapes, &[(0.0, 0.0), (120.0, 0.0), (0.0, 0.0)]),
            "{}",
            l.name
        );
    }
}

#[test]
fn distribute_evens_out_the_gaps_between_the_outer_two() {
    for l in layouts() {
        let r = l.rotation();
        // Across: A ends at 140, C starts at 300, B is 60 wide: two gaps of 50, so B starts
        // at 190 (it was at 200).
        let shapes = [A, B, C];
        let items: Vec<Annotation> = shapes.iter().map(|s| s.make(&l)).collect();
        let refs: Vec<&Annotation> = items.iter().collect();
        assert_eq!(
            distribute(&refs, Axis::Horizontal, r),
            expect(&l, &items, &shapes, &[(0.0, 0.0), (-10.0, 0.0), (0.0, 0.0)]),
            "{}",
            l.name
        );
        // The order the markups are given in does not matter.
        let shuffled = [refs[2], refs[0], refs[1]];
        assert_eq!(
            distribute(&shuffled, Axis::Horizontal, r),
            expect(&l, &items, &shapes, &[(0.0, 0.0), (-10.0, 0.0), (0.0, 0.0)]),
        );

        // Down: with B lowered to y 250..310, the order is A, C, B. A ends at 130, B starts at
        // 250 and C is 40 tall: two gaps of 40, so C starts at 170 (it was at 120).
        let shapes = [A, B.shifted(0.0, 100.0), C];
        let items: Vec<Annotation> = shapes.iter().map(|s| s.make(&l)).collect();
        let refs: Vec<&Annotation> = items.iter().collect();
        assert_eq!(
            distribute(&refs, Axis::Vertical, r),
            expect(&l, &items, &shapes, &[(0.0, 0.0), (0.0, 0.0), (0.0, 50.0)]),
            "{}",
            l.name
        );

        // Four, overlapping: the gaps come out equal even when they are negative.
        let shapes = [
            Shape::Box(100.0, 100.0, 200.0, 120.0),
            Shape::Box(110.0, 200.0, 190.0, 220.0),
            Shape::Box(150.0, 300.0, 210.0, 320.0),
            Shape::Box(180.0, 100.0, 280.0, 120.0),
        ];
        let items: Vec<Annotation> = shapes.iter().map(|s| s.make(&l)).collect();
        let refs: Vec<&Annotation> = items.iter().collect();
        // Widths 100, 80, 60, 100; from 200 to 180 with 140 between: gaps of -160 / 3.
        let got = distribute(&refs, Axis::Horizontal, r);
        let mut view = pl_tools::SimpleView::new(vec![], origin());
        view.annotations = items.clone();
        view.apply(&got);
        let mut spans: Vec<(f64, f64)> = view
            .annotations
            .iter()
            .map(|a| {
                let b = a.geometry.bounds();
                let (p, q) = (
                    l.screen(b.origin()),
                    l.screen(pl_geom::Point::new(b.x1, b.y1)),
                );
                (p.x.min(q.x), p.x.max(q.x))
            })
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(spans[0], (100.0, 200.0), "{}", l.name);
        assert_eq!(spans[3], (180.0, 280.0), "{}", l.name);
        for w in spans.windows(2) {
            let gap = w[1].0 - w[0].1;
            assert!((gap + 160.0 / 3.0).abs() < 1e-9, "{}: {gap}", l.name);
        }

        // Two markups have no gaps to even out.
        assert_eq!(distribute(&refs[..2], Axis::Horizontal, r), []);
    }
}

#[test]
fn nudge_moves_each_markup_in_its_own_pages_directions() {
    // Page 0 is upright and page 1 is turned a quarter: one screen direction, two page
    // directions.
    let l = layouts()[1];
    let mut rig = Rig::new(&l, ToolKind::Select);
    let upright = rig.add(Annotation::square(
        0,
        pl_geom::Rect::new(10.0, 10.0, 20.0, 20.0),
        &origin(),
    ));
    let turned = rig.add(A.make(&l));
    let ids = [upright.clone(), turned.clone()];
    // Five to the right of the screen.
    let got = nudge(&rig.view, &ids, pl_geom::Vec2::new(5.0, 0.0));
    assert_eq!(
        got,
        [
            modify(
                rig.get(&upright),
                Geometry::Rect(pl_geom::Rect::new(15.0, 10.0, 25.0, 20.0))
            ),
            modify(rig.get(&turned), A.shifted(5.0, 0.0).geometry(&l)),
        ]
    );
    // Unknown ids are skipped; a zero nudge is nothing.
    assert_eq!(
        nudge(
            &rig.view,
            &[AnnotId::from("gone")],
            pl_geom::Vec2::new(5.0, 0.0)
        ),
        []
    );
    assert_eq!(nudge(&rig.view, &ids, pl_geom::Vec2::ZERO), []);
}

fn ids(names: &str) -> Vec<AnnotId> {
    names.chars().map(|c| AnnotId(c.to_string())).collect()
}

fn names(order: &[AnnotId]) -> String {
    order.iter().map(|id| id.as_str()).collect()
}

#[test]
fn stacking_order_goldens() {
    // Bottom to top.
    let order = ids("abcdef");
    let run = |f: fn(&[AnnotId], &[AnnotId]) -> Vec<AnnotId>, selected: &str| {
        names(&f(&order, &ids(selected)))
    };
    assert_eq!(run(bring_to_front, "b"), "acdefb");
    assert_eq!(
        run(bring_to_front, "eb"),
        "acdfbe",
        "they keep their own order"
    );
    assert_eq!(run(bring_to_front, "f"), "abcdef");
    assert_eq!(run(send_to_back, "e"), "eabcdf");
    assert_eq!(run(send_to_back, "eb"), "beacdf");
    assert_eq!(run(send_to_back, "a"), "abcdef");

    assert_eq!(run(bring_forward, "b"), "acbdef");
    assert_eq!(run(bring_forward, "bc"), "adbcef", "neighbours move as one");
    assert_eq!(run(bring_forward, "be"), "acbdfe");
    assert_eq!(run(bring_forward, "f"), "abcdef", "already on top");
    assert_eq!(run(bring_forward, "ef"), "abcdef");
    assert_eq!(run(bring_forward, "df"), "abcedf");

    assert_eq!(run(send_backward, "e"), "abcedf");
    assert_eq!(run(send_backward, "de"), "abdecf");
    assert_eq!(run(send_backward, "a"), "abcdef", "already at the bottom");
    assert_eq!(run(send_backward, "ab"), "abcdef");
    assert_eq!(run(send_backward, "ac"), "acbdef");

    // Nothing selected, unknown ids and an empty page change nothing.
    for f in [bring_to_front, send_to_back, bring_forward, send_backward] {
        assert_eq!(names(&f(&order, &[])), "abcdef");
        assert_eq!(names(&f(&order, &ids("xyz"))), "abcdef");
        assert_eq!(f(&[], &ids("a")), []);
        // Every operation is a permutation.
        let mut out = f(&order, &ids("bdf"));
        out.sort();
        assert_eq!(out, order);
    }
}

#[test]
fn a_reorder_intent_restacks_one_page_only() {
    let l = layouts()[0];
    let mut rig = Rig::new(&l, ToolKind::Select);
    let other = rig.add(Annotation::square(
        0,
        pl_geom::Rect::new(0.0, 0.0, 10.0, 10.0),
        &origin(),
    ));
    let a = rig.add(A.make(&l));
    let other_top = rig.add(Annotation::square(
        0,
        pl_geom::Rect::new(0.0, 0.0, 10.0, 10.0),
        &origin(),
    ));
    let b = rig.add(B.make(&l));
    let c = rig.add(C.make(&l));
    let on_page = |rig: &Rig, page| -> Vec<AnnotId> {
        rig.view
            .annotations(page)
            .iter()
            .map(|x| x.id.clone())
            .collect()
    };
    assert_eq!(on_page(&rig, PAGE), [a.clone(), b.clone(), c.clone()]);

    rig.view.selection = vec![a.clone()];
    let intent = reorder_intent(&rig.view, PAGE, bring_to_front).expect("a change");
    assert_eq!(
        intent,
        Intent::Reorder {
            page: PAGE,
            order: vec![b.clone(), c.clone(), a.clone()]
        }
    );
    rig.view.apply(&[intent]);
    assert_eq!(on_page(&rig, PAGE), [b.clone(), c.clone(), a.clone()]);
    assert_eq!(on_page(&rig, 0), [other.clone(), other_top.clone()]);
    // Already on top: no intent.
    assert_eq!(reorder_intent(&rig.view, PAGE, bring_to_front), None);
    assert_eq!(reorder_intent(&rig.view, PAGE, bring_forward), None);
    // The selection is on this page; the other page has nothing to restack.
    assert_eq!(reorder_intent(&rig.view, 0, send_to_back), None);

    let intent = reorder_intent(&rig.view, PAGE, send_backward).expect("a change");
    rig.view.apply(&[intent]);
    assert_eq!(on_page(&rig, PAGE), [b.clone(), a.clone(), c.clone()]);
    let intent = reorder_intent(&rig.view, PAGE, send_to_back).expect("a change");
    rig.view.apply(&[intent]);
    assert_eq!(on_page(&rig, PAGE), [a, b, c]);
    assert_eq!(on_page(&rig, 0), [other, other_top]);
}
