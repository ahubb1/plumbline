//! The app's own icons, drawn as a few strokes each, and the icon button that shows them.

use egui::{pos2, Pos2, Rect, Response, Role, Sense, Stroke, StrokeKind, Ui, Vec2, WidgetInfo};

/// The icons. Each is a handful of line segments inside a square.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    ChevronUp,
    ChevronDown,
    Minus,
    Plus,
    TurnRight,
    TurnLeft,
    SidePanel,
    SplitSide,
    SplitStack,
    Cross,
    Lens,
    /// Select: a pointer arrow.
    Pointer,
    /// Pan: four arrows from a centre.
    Pan,
    Rectangle,
    Ellipse,
    Line,
    Arrow,
    Polyline,
    Polygon,
    Cloud,
    Pen,
    Highlighter,
    Note,
    /// Snap to drawing content: a corner with a marker on it.
    Snap,
    /// The properties panel: three sliders.
    Sliders,
    /// Text Box: a frame with a capital T in it.
    TextBox,
    /// Typewriter: a capital T on a baseline with a caret.
    Typewriter,
    /// Callout: a small box with a leader to an arrow head.
    Callout,
    /// Cloud Box: a scalloped rectangle with a corner mark.
    CloudBox,
    /// Cloud Callout: a scalloped shape with a leader to a box.
    CloudCallout,
    /// Stamp: a slanted double frame.
    Stamp,
    /// Image: a frame with a hill and a sun.
    Picture,
    /// Highlight Text: a capital A under a band.
    TextHighlight,
    TextUnderline,
    TextStrikeOut,
    TextSquiggly,
    /// Eraser: a slanted block on a line.
    Eraser,
    /// The markups list: rows with a column rule.
    List,
    /// Calibrate: a measured span with a caret under its middle.
    Calibrate,
    /// Viewport: a dashed frame with a short scale bar in it.
    Viewport,
    /// Length: a dimension line with end ticks.
    MeasureLength,
    /// Polylength: a bent run with a dot at each vertex.
    Polylength,
    /// Perimeter: an outline with a square at each corner.
    Perimeter,
    /// Area: a shaded outline.
    MeasureArea,
    /// Cutout: a shaded outline with a hole.
    Cutout,
    /// Volume: a box drawn in three faces.
    Volume,
    /// Angle: two arms with an arc between them.
    Angle,
    /// Radius: a circle with a line from its centre to its edge.
    Radius,
    /// Diameter: a circle with a line across it through the centre.
    Diameter,
    /// Count: three dots.
    Count,
    /// Space: a room outline with a name line.
    Space,
    /// Link: two rounded links of a chain, overlapping.
    Link,
}

/// Paints an icon centred in `rect`.
pub fn paint(painter: &egui::Painter, rect: Rect, icon: Icon, stroke: Stroke) {
    let c = rect.center();
    let r = rect.width().min(rect.height()) * 0.27;
    let line = |a: Pos2, b: Pos2| {
        painter.line_segment([a, b], stroke);
    };
    match icon {
        Icon::ChevronUp => {
            painter.line(
                vec![
                    pos2(c.x - r, c.y + r * 0.5),
                    pos2(c.x, c.y - r * 0.5),
                    pos2(c.x + r, c.y + r * 0.5),
                ],
                stroke,
            );
        }
        Icon::ChevronDown => {
            painter.line(
                vec![
                    pos2(c.x - r, c.y - r * 0.5),
                    pos2(c.x, c.y + r * 0.5),
                    pos2(c.x + r, c.y - r * 0.5),
                ],
                stroke,
            );
        }
        Icon::Minus => line(pos2(c.x - r, c.y), pos2(c.x + r, c.y)),
        Icon::Plus => {
            line(pos2(c.x - r, c.y), pos2(c.x + r, c.y));
            line(pos2(c.x, c.y - r), pos2(c.x, c.y + r));
        }
        Icon::TurnRight | Icon::TurnLeft => {
            // Three quarters of a circle with an arrow head at its open end.
            let mirror = if icon == Icon::TurnLeft { -1.0 } else { 1.0 };
            let at = |deg: f32| {
                let a = deg.to_radians();
                pos2(c.x + mirror * r * a.cos(), c.y + r * a.sin())
            };
            let points: Vec<Pos2> = (0..=18).map(|i| at(-250.0 + 15.0 * i as f32)).collect();
            let end = at(20.0);
            painter.line(points, stroke);
            line(end, pos2(end.x - mirror * r * 0.75, end.y - r * 0.15));
            line(end, pos2(end.x + mirror * r * 0.2, end.y - r * 0.75));
        }
        Icon::SidePanel => {
            let b = Rect::from_center_size(c, Vec2::new(2.4 * r, 2.0 * r));
            painter.rect_stroke(b, 1.0, stroke, StrokeKind::Middle);
            let x = b.left() + b.width() * 0.36;
            line(pos2(x, b.top()), pos2(x, b.bottom()));
        }
        Icon::SplitSide => {
            let b = Rect::from_center_size(c, Vec2::new(2.4 * r, 2.0 * r));
            painter.rect_stroke(b, 1.0, stroke, StrokeKind::Middle);
            line(pos2(c.x, b.top()), pos2(c.x, b.bottom()));
        }
        Icon::SplitStack => {
            let b = Rect::from_center_size(c, Vec2::new(2.4 * r, 2.0 * r));
            painter.rect_stroke(b, 1.0, stroke, StrokeKind::Middle);
            line(pos2(b.left(), c.y), pos2(b.right(), c.y));
        }
        Icon::Cross => {
            let k = r * 0.8;
            line(pos2(c.x - k, c.y - k), pos2(c.x + k, c.y + k));
            line(pos2(c.x - k, c.y + k), pos2(c.x + k, c.y - k));
        }
        Icon::Lens => {
            let centre = pos2(c.x - r * 0.2, c.y - r * 0.2);
            painter.circle_stroke(centre, r * 0.75, stroke);
            let d = r * 0.75 * std::f32::consts::FRAC_1_SQRT_2;
            line(pos2(centre.x + d, centre.y + d), pos2(c.x + r, c.y + r));
        }
        Icon::Pointer => {
            // An arrow head pointing up-left with a short tail.
            let tip = pos2(c.x - r * 0.8, c.y - r);
            let outline = vec![
                tip,
                pos2(tip.x, tip.y + r * 1.7),
                pos2(tip.x + r * 0.5, tip.y + r * 1.25),
                pos2(tip.x + r * 0.95, tip.y + r * 2.0),
                pos2(tip.x + r * 1.25, tip.y + r * 1.85),
                pos2(tip.x + r * 0.8, tip.y + r * 1.1),
                pos2(tip.x + r * 1.4, tip.y + r * 1.05),
            ];
            painter.add(egui::Shape::closed_line(outline, stroke));
        }
        Icon::Pan => {
            for (dx, dy) in [(1.0_f32, 0.0_f32), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
                let end = pos2(c.x + dx * r * 1.15, c.y + dy * r * 1.15);
                line(pos2(c.x + dx * r * 0.25, c.y + dy * r * 0.25), end);
                let (px, py) = (-dy, dx);
                for side in [-1.0_f32, 1.0] {
                    line(
                        end,
                        pos2(
                            end.x - dx * r * 0.4 + px * side * r * 0.35,
                            end.y - dy * r * 0.4 + py * side * r * 0.35,
                        ),
                    );
                }
            }
        }
        Icon::Rectangle => {
            let b = Rect::from_center_size(c, Vec2::new(2.3 * r, 1.7 * r));
            painter.rect_stroke(b, 0.0, stroke, StrokeKind::Middle);
        }
        Icon::Ellipse => {
            painter.add(egui::Shape::ellipse_stroke(
                c,
                Vec2::new(1.2 * r, 0.85 * r),
                stroke,
            ));
        }
        Icon::Line => line(pos2(c.x - r, c.y + r * 0.8), pos2(c.x + r, c.y - r * 0.8)),
        Icon::Arrow => {
            let tip = pos2(c.x + r, c.y - r * 0.8);
            line(pos2(c.x - r, c.y + r * 0.8), tip);
            line(tip, pos2(tip.x - r * 0.8, tip.y + r * 0.1));
            line(tip, pos2(tip.x - r * 0.25, tip.y + r * 0.75));
        }
        Icon::Polyline => {
            painter.line(
                vec![
                    pos2(c.x - r * 1.1, c.y + r * 0.8),
                    pos2(c.x - r * 0.4, c.y - r * 0.7),
                    pos2(c.x + r * 0.3, c.y + r * 0.5),
                    pos2(c.x + r * 1.1, c.y - r * 0.8),
                ],
                stroke,
            );
        }
        Icon::Polygon => {
            painter.add(egui::Shape::closed_line(
                vec![
                    pos2(c.x - r * 0.2, c.y - r),
                    pos2(c.x + r * 1.1, c.y - r * 0.3),
                    pos2(c.x + r * 0.7, c.y + r),
                    pos2(c.x - r * 0.8, c.y + r * 0.8),
                    pos2(c.x - r * 1.1, c.y - r * 0.2),
                ],
                stroke,
            ));
        }
        Icon::Cloud => {
            // Scallops round a rectangle: arcs bulging outwards along each side.
            let b = Rect::from_center_size(c, Vec2::new(1.9 * r, 1.3 * r));
            let arc = |from: Pos2, to: Pos2| {
                let mid = from + (to - from) * 0.5;
                let half = (to - from) * 0.5;
                let out = egui::vec2(half.y, -half.x);
                let points: Vec<Pos2> = (0..=8)
                    .map(|i| {
                        let t = std::f32::consts::PI * i as f32 / 8.0;
                        mid - half * t.cos() + out * t.sin()
                    })
                    .collect();
                painter.line(points, stroke);
            };
            let corners = [
                b.left_top(),
                b.right_top(),
                b.right_bottom(),
                b.left_bottom(),
            ];
            for i in 0..4 {
                let (a, z) = (corners[i], corners[(i + 1) % 4]);
                let pieces = if i % 2 == 0 { 3 } else { 2 };
                for k in 0..pieces {
                    let from = a + (z - a) * (k as f32 / pieces as f32);
                    let to = a + (z - a) * ((k + 1) as f32 / pieces as f32);
                    arc(from, to);
                }
            }
        }
        Icon::Pen => {
            // A freehand squiggle.
            let points: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let t = i as f32 / 16.0;
                    pos2(
                        c.x - r * 1.1 + 2.2 * r * t,
                        c.y + r * 0.75 * (t * std::f32::consts::TAU * 1.25).sin() * (1.0 - 0.4 * t),
                    )
                })
                .collect();
            painter.line(points, stroke);
        }
        Icon::Highlighter => {
            // A broad translucent band under a baseline.
            let band = Rect::from_center_size(c, Vec2::new(2.3 * r, 0.9 * r));
            painter.rect_filled(band, 1.0, stroke.color.gamma_multiply(0.3));
            line(
                pos2(band.left(), band.bottom() + r * 0.35),
                pos2(band.right(), band.bottom() + r * 0.35),
            );
        }
        Icon::Note => {
            // A sheet with a folded corner and two lines of writing.
            let b = Rect::from_center_size(c, Vec2::new(1.9 * r, 2.1 * r));
            let fold = r * 0.6;
            painter.add(egui::Shape::closed_line(
                vec![
                    b.left_top(),
                    pos2(b.right() - fold, b.top()),
                    pos2(b.right(), b.top() + fold),
                    b.right_bottom(),
                    b.left_bottom(),
                ],
                stroke,
            ));
            line(
                pos2(b.left() + r * 0.4, c.y),
                pos2(b.right() - r * 0.4, c.y),
            );
            line(
                pos2(b.left() + r * 0.4, c.y + r * 0.5),
                pos2(b.right() - r * 0.4, c.y + r * 0.5),
            );
        }
        Icon::Snap => {
            let corner = pos2(c.x - r * 0.3, c.y + r * 0.3);
            line(pos2(corner.x, c.y - r * 1.1), corner);
            line(corner, pos2(c.x + r * 1.1, corner.y));
            let marker = Rect::from_center_size(corner, Vec2::splat(r * 0.9));
            painter.rect_stroke(marker, 0.0, stroke, StrokeKind::Middle);
        }
        Icon::Sliders => {
            for (row, knob) in [(-0.8_f32, 0.3_f32), (0.0, -0.4), (0.8, 0.5)] {
                let y = c.y + row * r;
                line(pos2(c.x - r * 1.1, y), pos2(c.x + r * 1.1, y));
                painter.circle_filled(pos2(c.x + knob * r, y), r * 0.26, stroke.color);
            }
        }
        Icon::TextBox => {
            let b = Rect::from_center_size(c, Vec2::new(2.4 * r, 2.0 * r));
            painter.rect_stroke(b, 1.0, stroke, StrokeKind::Middle);
            line(
                pos2(c.x - r * 0.6, c.y - r * 0.5),
                pos2(c.x + r * 0.6, c.y - r * 0.5),
            );
            line(pos2(c.x, c.y - r * 0.5), pos2(c.x, c.y + r * 0.6));
        }
        Icon::Typewriter => {
            line(
                pos2(c.x - r * 1.0, c.y - r * 0.9),
                pos2(c.x + r * 0.3, c.y - r * 0.9),
            );
            line(
                pos2(c.x - r * 0.35, c.y - r * 0.9),
                pos2(c.x - r * 0.35, c.y + r * 0.6),
            );
            line(
                pos2(c.x - r * 1.1, c.y + r * 1.0),
                pos2(c.x + r * 1.1, c.y + r * 1.0),
            );
            line(
                pos2(c.x + r * 0.8, c.y - r * 0.6),
                pos2(c.x + r * 0.8, c.y + r * 0.6),
            );
        }
        Icon::Callout => {
            let b = Rect::from_min_max(
                pos2(c.x - r * 0.2, c.y - r * 1.0),
                pos2(c.x + r * 1.2, c.y + r * 0.1),
            );
            painter.rect_stroke(b, 0.0, stroke, StrokeKind::Middle);
            let tip = pos2(c.x - r * 1.1, c.y + r * 1.0);
            painter.line(
                vec![
                    pos2(b.left(), c.y - r * 0.45),
                    pos2(c.x - r * 0.7, c.y - r * 0.45),
                    tip,
                ],
                stroke,
            );
            line(tip, pos2(tip.x + r * 0.05, tip.y - r * 0.6));
            line(tip, pos2(tip.x + r * 0.55, tip.y - r * 0.25));
        }
        Icon::CloudBox | Icon::CloudCallout => {
            let callout = icon == Icon::CloudCallout;
            let b = if callout {
                Rect::from_min_max(
                    pos2(c.x - r * 1.2, c.y - r * 0.1),
                    pos2(c.x + r * 0.2, c.y + r * 1.0),
                )
            } else {
                Rect::from_center_size(c, Vec2::new(2.0 * r, 1.5 * r))
            };
            let arc = |from: Pos2, to: Pos2| {
                let mid = from + (to - from) * 0.5;
                let half = (to - from) * 0.5;
                let out = egui::vec2(half.y, -half.x);
                let points: Vec<Pos2> = (0..=6)
                    .map(|i| {
                        let t = std::f32::consts::PI * i as f32 / 6.0;
                        mid - half * t.cos() + out * t.sin()
                    })
                    .collect();
                painter.line(points, stroke);
            };
            let corners = [
                b.left_top(),
                b.right_top(),
                b.right_bottom(),
                b.left_bottom(),
            ];
            for i in 0..4 {
                let (a, z) = (corners[i], corners[(i + 1) % 4]);
                let mid = a + (z - a) * 0.5;
                arc(a, mid);
                arc(mid, z);
            }
            if callout {
                let note = Rect::from_min_max(
                    pos2(c.x + r * 0.3, c.y - r * 1.1),
                    pos2(c.x + r * 1.2, c.y - r * 0.4),
                );
                painter.rect_stroke(note, 0.0, stroke, StrokeKind::Middle);
                line(
                    note.left_bottom(),
                    pos2(b.right() - r * 0.1, b.top() + r * 0.1),
                );
            } else {
                // The cross that tells it from the freeform cloud: it is dragged, not clicked.
                let k = r * 0.35;
                line(pos2(c.x - k, c.y), pos2(c.x + k, c.y));
                line(pos2(c.x, c.y - k), pos2(c.x, c.y + k));
            }
        }
        Icon::Stamp => {
            // A frame within a frame, tilted a little.
            let tilt = |p: Pos2| {
                let (dx, dy) = (p.x - c.x, p.y - c.y);
                let (sin, cos) = (-0.2_f32).sin_cos();
                pos2(c.x + dx * cos - dy * sin, c.y + dx * sin + dy * cos)
            };
            for (w, h) in [(1.25_f32, 0.8_f32), (0.95, 0.5)] {
                let quad = vec![
                    tilt(pos2(c.x - r * w, c.y - r * h)),
                    tilt(pos2(c.x + r * w, c.y - r * h)),
                    tilt(pos2(c.x + r * w, c.y + r * h)),
                    tilt(pos2(c.x - r * w, c.y + r * h)),
                ];
                painter.add(egui::Shape::closed_line(quad, stroke));
            }
            line(
                tilt(pos2(c.x - r * 0.55, c.y)),
                tilt(pos2(c.x + r * 0.55, c.y)),
            );
        }
        Icon::Picture => {
            let b = Rect::from_center_size(c, Vec2::new(2.4 * r, 1.9 * r));
            painter.rect_stroke(b, 1.0, stroke, StrokeKind::Middle);
            painter.line(
                vec![
                    pos2(b.left() + r * 0.15, b.bottom() - r * 0.2),
                    pos2(c.x - r * 0.3, c.y - r * 0.1),
                    pos2(c.x + r * 0.25, c.y + r * 0.45),
                    pos2(c.x + r * 0.6, c.y + r * 0.1),
                    pos2(b.right() - r * 0.15, b.bottom() - r * 0.2),
                ],
                stroke,
            );
            painter.circle_stroke(pos2(c.x + r * 0.6, c.y - r * 0.45), r * 0.2, stroke);
        }
        Icon::TextHighlight | Icon::TextUnderline | Icon::TextStrikeOut | Icon::TextSquiggly => {
            // A capital A, marked the way the tool marks text.
            let apex = pos2(c.x, c.y - r * 1.0);
            let left = pos2(c.x - r * 0.7, c.y + r * 0.55);
            let right = pos2(c.x + r * 0.7, c.y + r * 0.55);
            match icon {
                Icon::TextHighlight => {
                    let band = Rect::from_min_max(
                        pos2(c.x - r * 1.15, c.y - r * 0.35),
                        pos2(c.x + r * 1.15, c.y + r * 0.7),
                    );
                    painter.rect_filled(band, 1.0, stroke.color.gamma_multiply(0.28));
                }
                Icon::TextUnderline => {
                    line(
                        pos2(c.x - r * 1.1, c.y + r * 1.0),
                        pos2(c.x + r * 1.1, c.y + r * 1.0),
                    );
                }
                Icon::TextStrikeOut => {
                    line(
                        pos2(c.x - r * 1.15, c.y - r * 0.1),
                        pos2(c.x + r * 1.15, c.y - r * 0.1),
                    );
                }
                _ => {
                    let points: Vec<Pos2> = (0..=8)
                        .map(|i| {
                            let t = i as f32 / 8.0;
                            let up = if i % 2 == 0 { 0.85 } else { 1.15 };
                            pos2(c.x - r * 1.1 + 2.2 * r * t, c.y + r * up)
                        })
                        .collect();
                    painter.line(points, stroke);
                }
            }
            painter.line(vec![left, apex, right], stroke);
            if icon != Icon::TextStrikeOut {
                line(
                    pos2(c.x - r * 0.4, c.y + r * 0.05),
                    pos2(c.x + r * 0.4, c.y + r * 0.05),
                );
            }
        }
        Icon::Eraser => {
            let quad = vec![
                pos2(c.x - r * 0.2, c.y - r * 1.0),
                pos2(c.x + r * 1.0, c.y - r * 0.1),
                pos2(c.x + r * 0.2, c.y + r * 0.7),
                pos2(c.x - r * 1.0, c.y - r * 0.2),
            ];
            painter.add(egui::Shape::closed_line(quad, stroke));
            line(
                pos2(c.x - r * 0.45, c.y - r * 0.55),
                pos2(c.x + r * 0.65, c.y + r * 0.35),
            );
            line(
                pos2(c.x - r * 1.1, c.y + r * 1.0),
                pos2(c.x + r * 1.1, c.y + r * 1.0),
            );
        }
        Icon::List => {
            let b = Rect::from_center_size(c, Vec2::new(2.4 * r, 2.0 * r));
            painter.rect_stroke(b, 1.0, stroke, StrokeKind::Middle);
            for k in [-0.35_f32, 0.3] {
                line(pos2(b.left(), c.y + k * r), pos2(b.right(), c.y + k * r));
            }
            let x = b.left() + b.width() * 0.32;
            line(pos2(x, b.top()), pos2(x, b.bottom()));
        }
        Icon::Calibrate => {
            let y = c.y - r * 0.2;
            line(pos2(c.x - r * 1.1, y), pos2(c.x + r * 1.1, y));
            for x in [c.x - r * 1.1, c.x + r * 1.1] {
                line(pos2(x, y - r * 0.5), pos2(x, y + r * 0.5));
            }
            painter.line(
                vec![
                    pos2(c.x - r * 0.45, c.y + r * 1.0),
                    pos2(c.x, c.y + r * 0.35),
                    pos2(c.x + r * 0.45, c.y + r * 1.0),
                ],
                stroke,
            );
        }
        Icon::Viewport => {
            let b = Rect::from_center_size(c, Vec2::new(2.4 * r, 2.0 * r));
            let corners = [
                b.left_top(),
                b.right_top(),
                b.right_bottom(),
                b.left_bottom(),
                b.left_top(),
            ];
            for shape in egui::Shape::dashed_line(&corners, stroke, r * 0.4, r * 0.3) {
                painter.add(shape);
            }
            let y = c.y + r * 0.35;
            line(pos2(c.x - r * 0.6, y), pos2(c.x + r * 0.6, y));
            for x in [c.x - r * 0.6, c.x, c.x + r * 0.6] {
                line(pos2(x, y - r * 0.3), pos2(x, y));
            }
        }
        Icon::MeasureLength => {
            let y = c.y + r * 0.1;
            let (a, z) = (pos2(c.x - r * 1.1, y), pos2(c.x + r * 1.1, y));
            line(a, z);
            for (end, dir) in [(a, 1.0_f32), (z, -1.0)] {
                line(end, pos2(end.x + dir * r * 0.45, end.y - r * 0.3));
                line(end, pos2(end.x + dir * r * 0.45, end.y + r * 0.3));
                line(pos2(end.x, end.y - r * 0.75), pos2(end.x, end.y + r * 0.45));
            }
        }
        Icon::Polylength => {
            let points = vec![
                pos2(c.x - r * 1.1, c.y + r * 0.8),
                pos2(c.x - r * 0.3, c.y - r * 0.6),
                pos2(c.x + r * 0.4, c.y + r * 0.4),
                pos2(c.x + r * 1.1, c.y - r * 0.8),
            ];
            painter.line(points.clone(), stroke);
            for p in points {
                painter.circle_filled(p, r * 0.2, stroke.color);
            }
        }
        Icon::Perimeter | Icon::MeasureArea | Icon::Cutout => {
            let outline = vec![
                pos2(c.x - r * 1.1, c.y - r * 0.9),
                pos2(c.x + r * 0.4, c.y - r * 0.9),
                pos2(c.x + r * 1.1, c.y - r * 0.1),
                pos2(c.x + r * 1.1, c.y + r * 0.9),
                pos2(c.x - r * 1.1, c.y + r * 0.9),
            ];
            if icon == Icon::MeasureArea {
                painter.add(egui::Shape::convex_polygon(
                    outline.clone(),
                    stroke.color.gamma_multiply(0.25),
                    Stroke::NONE,
                ));
            }
            painter.add(egui::Shape::closed_line(outline.clone(), stroke));
            match icon {
                Icon::Perimeter => {
                    for p in outline {
                        let mark = Rect::from_center_size(p, Vec2::splat(r * 0.38));
                        painter.rect_filled(mark, 0.0, stroke.color);
                    }
                }
                Icon::Cutout => {
                    let hole = Rect::from_center_size(
                        pos2(c.x - r * 0.1, c.y + r * 0.1),
                        Vec2::new(r * 0.9, r * 0.75),
                    );
                    painter.rect_stroke(hole, 0.0, stroke, StrokeKind::Middle);
                    // Hatching outside the hole says what is kept.
                    for k in [0.35_f32, 0.7] {
                        line(
                            pos2(c.x - r * 1.1, c.y - r * 0.9 + k * r),
                            pos2(c.x - r * 1.1 + k * r, c.y - r * 0.9),
                        );
                    }
                }
                _ => {}
            }
        }
        Icon::Volume => {
            let d = r * 0.55;
            let front = Rect::from_min_max(
                pos2(c.x - r * 1.1, c.y - r * 0.5),
                pos2(c.x + r * 0.5, c.y + r * 1.0),
            );
            painter.rect_stroke(front, 0.0, stroke, StrokeKind::Middle);
            let back = front.translate(Vec2::new(d, -d));
            painter.line(
                vec![
                    front.left_top(),
                    back.left_top(),
                    back.right_top(),
                    back.right_bottom(),
                    front.right_bottom(),
                ],
                stroke,
            );
            line(front.right_top(), back.right_top());
        }
        Icon::Angle => {
            let vertex = pos2(c.x - r * 1.0, c.y + r * 0.9);
            line(vertex, pos2(c.x + r * 1.15, c.y + r * 0.9));
            let tip = pos2(c.x + r * 0.55, c.y - r * 1.05);
            line(vertex, tip);
            let (dx, dy) = (tip.x - vertex.x, tip.y - vertex.y);
            let end = dy.atan2(dx);
            let radius = r * 1.05;
            let points: Vec<Pos2> = (0..=10)
                .map(|i| {
                    let a = end * i as f32 / 10.0;
                    pos2(vertex.x + radius * a.cos(), vertex.y + radius * a.sin())
                })
                .collect();
            painter.line(points, stroke);
        }
        Icon::Radius | Icon::Diameter => {
            painter.circle_stroke(c, r * 1.05, stroke);
            painter.circle_filled(c, r * 0.17, stroke.color);
            let d = r * 1.05 * std::f32::consts::FRAC_1_SQRT_2;
            let start = if icon == Icon::Diameter {
                pos2(c.x - d, c.y + d)
            } else {
                c
            };
            line(start, pos2(c.x + d, c.y - d));
        }
        Icon::Link => {
            // Two links of a chain: slanted capsules, overlapping in the middle.
            let d = egui::vec2(0.8, -0.6);
            let n = egui::vec2(0.6, 0.8);
            let (half, w) = (r * 0.55, r * 0.42);
            for shift in [-0.5_f32, 0.5] {
                let centre = c + d * (shift * r * 1.15);
                let mut points = Vec::with_capacity(20);
                for i in 0..=9 {
                    let t = std::f32::consts::PI * (-0.5 + i as f32 / 9.0);
                    points.push(centre + d * half + (d * t.cos() + n * t.sin()) * w);
                }
                for i in 0..=9 {
                    let t = std::f32::consts::PI * (0.5 + i as f32 / 9.0);
                    points.push(centre - d * half + (d * t.cos() + n * t.sin()) * w);
                }
                painter.add(egui::Shape::closed_line(points, stroke));
            }
        }
        Icon::Count => {
            for (dx, dy) in [(-0.7_f32, 0.55_f32), (0.7, 0.55), (0.0, -0.65)] {
                painter.circle_filled(pos2(c.x + dx * r, c.y + dy * r), r * 0.36, stroke.color);
            }
        }
        Icon::Space => {
            painter.add(egui::Shape::closed_line(
                vec![
                    pos2(c.x - r * 1.1, c.y - r * 1.0),
                    pos2(c.x + r * 1.1, c.y - r * 1.0),
                    pos2(c.x + r * 1.1, c.y + r * 0.2),
                    pos2(c.x + r * 0.2, c.y + r * 0.2),
                    pos2(c.x + r * 0.2, c.y + r * 1.0),
                    pos2(c.x - r * 1.1, c.y + r * 1.0),
                ],
                stroke,
            ));
            line(
                pos2(c.x - r * 0.7, c.y - r * 0.35),
                pos2(c.x + r * 0.5, c.y - r * 0.35),
            );
        }
    }
}

/// A square button that shows an icon. `name` is its accessible name and its tooltip.
pub fn icon_button(ui: &mut Ui, icon: Icon, name: &str, selected: bool) -> Response {
    icon_button_tip(ui, icon, name, name, selected)
}

/// As [`icon_button`], with a tooltip that says more than the accessible name (a shortcut key,
/// say).
pub fn icon_button_tip(
    ui: &mut Ui,
    icon: Icon,
    name: &str,
    tooltip: &str,
    selected: bool,
) -> Response {
    let side = ui.spacing().interact_size.y;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::selected(Role::Button, enabled, selected, name));
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact_selectable(&response, selected);
        if selected || response.hovered() || response.is_pointer_button_down_on() {
            ui.painter().rect(
                rect,
                visuals.corner_radius,
                visuals.weak_bg_fill,
                visuals.bg_stroke,
                StrokeKind::Inside,
            );
        }
        let mut stroke = Stroke::new(1.4, visuals.fg_stroke.color);
        if !enabled {
            stroke.color = stroke.color.gamma_multiply(0.45);
        }
        paint(ui.painter(), rect, icon, stroke);
    }
    response.on_hover_text(tooltip)
}
