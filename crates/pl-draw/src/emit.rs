//! Appearance-stream emitter: turns a [`DrawList`] into content-stream bytes plus a plain
//! description of the resources the stream refers to.
//!
//! Nothing here builds PDF objects. `pl-pdf` turns an [`Appearance`] into a form XObject (and
//! its nested XObjects) with lopdf; the tests of this crate do the same with pdf-writer.
//!
//! The content is written in page coordinates, so with `/Matrix` the identity and the
//! annotation's `/Rect` equal to [`Appearance::bbox`] a viewer places it exactly where the
//! rasteriser does.

use std::sync::Arc;

use kurbo::{Affine, BezPath, PathEl, Rect};
use pdf_writer::types::{LineCapStyle, LineJoinStyle};
use pdf_writer::{Content, Name, Str};

use crate::list::{ops_bounds, DrawList, DrawOp, Group, ImageOp, PathOp, RgbaImage, TextRun};
use crate::style::{clamp_opacity, BlendMode, FillRule, LineCap, LineJoin, Rgb, Stroke};
use crate::text::{winansi_encode, FontStyle};
use crate::DrawError;

/// A form XObject, described as data: an annotation's `/AP /N` stream.
#[derive(Clone, Debug, PartialEq)]
pub struct Appearance {
    /// The uncompressed content stream.
    pub content: Vec<u8>,
    /// `/BBox`, in the coordinates of the content (page space for the top-level appearance).
    pub bbox: Rect,
    /// `/Matrix`. Always the identity for what this emitter produces.
    pub matrix: Affine,
    /// What `/Resources` must hold.
    pub resources: Resources,
}

/// The resource dictionary of one content stream. Names are unique within their list and are
/// the names the stream uses (without the leading slash).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resources {
    /// `/ExtGState` entries.
    pub ext_gstates: Vec<ExtGState>,
    /// `/Font` entries.
    pub fonts: Vec<FontResource>,
    /// `/XObject` entries.
    pub xobjects: Vec<XObjectResource>,
}

impl Resources {
    pub fn is_empty(&self) -> bool {
        self.ext_gstates.is_empty() && self.fonts.is_empty() && self.xobjects.is_empty()
    }
}

/// A graphics-state dictionary: `<< /Type /ExtGState /CA .. /ca .. /BM .. >>`.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtGState {
    pub name: String,
    /// `/CA`, the stroking alpha.
    pub stroke_alpha: f64,
    /// `/ca`, the non-stroking alpha.
    pub fill_alpha: f64,
    /// `/BM`.
    pub blend: BlendMode,
}

/// A standard-14 font: `<< /Type /Font /Subtype /Type1 /BaseFont .. /Encoding /WinAnsiEncoding >>`.
#[derive(Clone, Debug, PartialEq)]
pub struct FontResource {
    pub name: String,
    pub style: FontStyle,
    /// `/BaseFont`, for example `Helvetica-Bold`.
    pub base_font: &'static str,
    /// `/Encoding`: always `WinAnsiEncoding`.
    pub encoding: &'static str,
}

/// One `/XObject` entry.
#[derive(Clone, Debug, PartialEq)]
pub struct XObjectResource {
    pub name: String,
    pub object: XObject,
}

#[derive(Clone, Debug, PartialEq)]
pub enum XObject {
    Form(FormXObject),
    Image(ImageXObject),
}

/// A nested form XObject: a translucent or blended group.
#[derive(Clone, Debug, PartialEq)]
pub struct FormXObject {
    pub form: Appearance,
    /// When set the form needs `/Group << /S /Transparency /I <isolated> >>`.
    pub group: Option<TransparencyGroup>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransparencyGroup {
    /// `/I`.
    pub isolated: bool,
    /// `/K`.
    pub knockout: bool,
}

/// An image XObject: `/Width`, `/Height`, `/ColorSpace /DeviceRGB`, `/BitsPerComponent 8`, and
/// an `/SMask` (DeviceGray, 8 bits) when the image has transparency. The sample data is
/// unfiltered; the writer may compress it.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageXObject {
    pub width: u32,
    pub height: u32,
    /// `width * height * 3` bytes, top row first.
    pub rgb: Vec<u8>,
    /// `width * height` bytes of alpha, or `None` for an opaque image.
    pub alpha: Option<Vec<u8>>,
}

/// Emits the list with `/BBox` set to its [bounds](DrawList::bounds) (an empty box for a list
/// that paints nothing).
pub fn emit(list: &DrawList) -> Result<Appearance, DrawError> {
    let bbox = list.bounds().unwrap_or(Rect::ZERO);
    emit_with_bbox(list, bbox)
}

/// Emits the list with a `/BBox` chosen by the caller, for example the annotation's `/Rect`.
/// Viewers clip the appearance to it.
pub fn emit_with_bbox(list: &DrawList, bbox: Rect) -> Result<Appearance, DrawError> {
    list.validate()?;
    if ![bbox.x0, bbox.y0, bbox.x1, bbox.y1]
        .iter()
        .all(|v| v.is_finite())
    {
        return Err(DrawError::InvalidNumber);
    }
    emit_form(&list.ops, bbox.abs())
}

fn emit_form(ops: &[DrawOp], bbox: Rect) -> Result<Appearance, DrawError> {
    let mut e = Emitter {
        content: Content::new(),
        resources: Resources::default(),
        images: Vec::new(),
    };
    e.ops(ops)?;
    Ok(Appearance {
        content: e.content.finish().into_vec(),
        bbox,
        matrix: Affine::IDENTITY,
        resources: e.resources,
    })
}

struct Emitter {
    content: Content,
    resources: Resources,
    /// Images already emitted in this stream, so one picture used twice is stored once.
    images: Vec<(Arc<RgbaImage>, String)>,
}

fn n(v: f64) -> f32 {
    v as f32
}

impl Emitter {
    fn ops(&mut self, ops: &[DrawOp]) -> Result<(), DrawError> {
        for op in ops {
            match op {
                DrawOp::Path(p) => self.path(p),
                DrawOp::Text(t) => self.text(t)?,
                DrawOp::Image(i) => self.image(i),
                DrawOp::Group(g) => self.group(g)?,
            }
        }
        Ok(())
    }

    /// Emits `/GSn gs` when the op is not plain opaque Normal, registering the state.
    fn graphics_state(&mut self, opacity: f64, blend: BlendMode) {
        if opacity >= 1.0 && blend == BlendMode::Normal {
            return;
        }
        let existing = self
            .resources
            .ext_gstates
            .iter()
            .find(|g| g.stroke_alpha == opacity && g.fill_alpha == opacity && g.blend == blend);
        let name = match existing {
            Some(g) => g.name.clone(),
            None => {
                let name = format!("PlGs{}", self.resources.ext_gstates.len());
                self.resources.ext_gstates.push(ExtGState {
                    name: name.clone(),
                    stroke_alpha: opacity,
                    fill_alpha: opacity,
                    blend,
                });
                name
            }
        };
        self.content.set_parameters(Name(name.as_bytes()));
    }

    fn construct(&mut self, path: &BezPath) {
        let c = &mut self.content;
        let mut current = kurbo::Point::ZERO;
        let mut start = kurbo::Point::ZERO;
        for el in path.elements() {
            match *el {
                PathEl::MoveTo(p) => {
                    c.move_to(n(p.x), n(p.y));
                    current = p;
                    start = p;
                }
                PathEl::LineTo(p) => {
                    c.line_to(n(p.x), n(p.y));
                    current = p;
                }
                PathEl::QuadTo(q, p) => {
                    // PDF has no quadratic segment: raise to the equivalent cubic.
                    let c1 = current + (q - current) * (2.0 / 3.0);
                    let c2 = p + (q - p) * (2.0 / 3.0);
                    c.cubic_to(n(c1.x), n(c1.y), n(c2.x), n(c2.y), n(p.x), n(p.y));
                    current = p;
                }
                PathEl::CurveTo(c1, c2, p) => {
                    c.cubic_to(n(c1.x), n(c1.y), n(c2.x), n(c2.y), n(p.x), n(p.y));
                    current = p;
                }
                PathEl::ClosePath => {
                    c.close_path();
                    current = start;
                }
            }
        }
    }

    fn fill_color(&mut self, color: Rgb) {
        let c = color.clamped();
        self.content.set_fill_rgb(c.r, c.g, c.b);
    }

    fn stroke_state(&mut self, stroke: &Stroke) {
        let c = stroke.color.clamped();
        let content = &mut self.content;
        content.set_stroke_rgb(c.r, c.g, c.b);
        content.set_line_width(n(stroke.width.max(0.0)));
        content.set_line_cap(match stroke.cap {
            LineCap::Butt => LineCapStyle::ButtCap,
            LineCap::Round => LineCapStyle::RoundCap,
            LineCap::Square => LineCapStyle::ProjectingSquareCap,
        });
        content.set_line_join(match stroke.join {
            LineJoin::Miter => LineJoinStyle::MiterJoin,
            LineJoin::Round => LineJoinStyle::RoundJoin,
            LineJoin::Bevel => LineJoinStyle::BevelJoin,
        });
        content.set_miter_limit(n(stroke.miter_limit.max(1.0)));
        if let Some((array, phase)) = stroke.effective_dash() {
            content.set_dash_pattern(array.iter().map(|v| n(*v)), n(phase));
        }
    }

    fn path(&mut self, op: &PathOp) {
        let opacity = clamp_opacity(op.opacity);
        let has_segments = op.path.elements().len() > 1;
        if !has_segments || opacity <= 0.0 || (op.fill.is_none() && op.stroke.is_none()) {
            return;
        }
        self.content.save_state();
        self.graphics_state(opacity, op.blend);
        if let Some(fill) = &op.fill {
            self.fill_color(fill.color);
        }
        if let Some(stroke) = &op.stroke {
            self.stroke_state(stroke);
        }
        let plain = opacity >= 1.0 && op.blend == BlendMode::Normal;
        match (&op.fill, &op.stroke) {
            (Some(fill), Some(_)) if plain => {
                self.construct(&op.path);
                match fill.rule {
                    FillRule::NonZero => self.content.fill_nonzero_and_stroke(),
                    FillRule::EvenOdd => self.content.fill_even_odd_and_stroke(),
                };
            }
            (fill, stroke) => {
                // Translucent or blended: PDF says a combined fill-and-stroke is painted as a
                // knockout group, which viewers honour unevenly. Painting the fill and then the
                // stroke as two objects means the same thing everywhere, and is what the
                // rasteriser does.
                if let Some(fill) = fill {
                    self.construct(&op.path);
                    match fill.rule {
                        FillRule::NonZero => self.content.fill_nonzero(),
                        FillRule::EvenOdd => self.content.fill_even_odd(),
                    };
                }
                if stroke.is_some() {
                    self.construct(&op.path);
                    self.content.stroke();
                }
            }
        }
        self.content.restore_state();
    }

    fn text(&mut self, run: &TextRun) -> Result<(), DrawError> {
        let bytes = winansi_encode(&run.text)?;
        let opacity = clamp_opacity(run.opacity);
        if bytes.is_empty() || opacity <= 0.0 {
            return Ok(());
        }
        let font = match self.resources.fonts.iter().find(|f| f.style == run.style) {
            Some(f) => f.name.clone(),
            None => {
                let name = format!("PlF{}", self.resources.fonts.len());
                self.resources.fonts.push(FontResource {
                    name: name.clone(),
                    style: run.style,
                    base_font: run.style.base_font(),
                    encoding: "WinAnsiEncoding",
                });
                name
            }
        };
        self.content.save_state();
        self.graphics_state(opacity, run.blend);
        self.fill_color(run.color);
        self.content.begin_text();
        self.content.set_font(Name(font.as_bytes()), n(run.size));
        self.content
            .set_text_matrix([1.0, 0.0, 0.0, 1.0, n(run.origin.x), n(run.origin.y)]);
        self.content.show(Str(&bytes));
        self.content.end_text();
        self.content.restore_state();
        Ok(())
    }

    fn xobject_name(&self) -> String {
        format!("PlX{}", self.resources.xobjects.len())
    }

    fn image(&mut self, op: &ImageOp) {
        let opacity = clamp_opacity(op.opacity);
        let rect = op.rect.abs();
        if opacity <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let known = self.images.iter().find(|(i, _)| Arc::ptr_eq(i, &op.image));
        let name = match known {
            Some((_, name)) => name.clone(),
            None => {
                let name = self.xobject_name();
                let px = op.image.pixels();
                let rgb = px
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|p| [p[0], p[1], p[2]])
                    .collect();
                let alpha = (!op.image.is_opaque())
                    .then(|| px.as_chunks::<4>().0.iter().map(|p| p[3]).collect());
                self.resources.xobjects.push(XObjectResource {
                    name: name.clone(),
                    object: XObject::Image(ImageXObject {
                        width: op.image.width(),
                        height: op.image.height(),
                        rgb,
                        alpha,
                    }),
                });
                self.images.push((op.image.clone(), name.clone()));
                name
            }
        };
        self.content.save_state();
        self.graphics_state(opacity, op.blend);
        // An image fills the unit square with its top row at y = 1.
        self.content.transform([
            n(rect.width()),
            0.0,
            0.0,
            n(rect.height()),
            n(rect.x0),
            n(rect.y0),
        ]);
        self.content.x_object(Name(name.as_bytes()));
        self.content.restore_state();
    }

    fn group(&mut self, group: &Group) -> Result<(), DrawError> {
        let opacity = clamp_opacity(group.opacity);
        if group.ops.is_empty() || opacity <= 0.0 {
            return Ok(());
        }
        let m = group.transform.as_coeffs();
        let matrix = [n(m[0]), n(m[1]), n(m[2]), n(m[3]), n(m[4]), n(m[5])];
        let identity = group.transform == Affine::IDENTITY;
        let clip = group.clip.map(|c| c.abs());
        if !group.is_layer() {
            if identity && clip.is_none() {
                return self.ops(&group.ops);
            }
            self.content.save_state();
            if !identity {
                self.content.transform(matrix);
            }
            if let Some(c) = clip {
                self.clip_to(c);
            }
            self.ops(&group.ops)?;
            self.content.restore_state();
            return Ok(());
        }
        let Some(bounds) = ops_bounds(&group.ops) else {
            return Ok(());
        };
        let form = emit_form(&group.ops, bounds)?;
        if form.content.is_empty() {
            return Ok(());
        }
        let name = self.xobject_name();
        self.resources.xobjects.push(XObjectResource {
            name: name.clone(),
            object: XObject::Form(FormXObject {
                form,
                group: Some(TransparencyGroup {
                    isolated: true,
                    knockout: false,
                }),
            }),
        });
        self.content.save_state();
        self.graphics_state(opacity, group.blend);
        if !identity {
            self.content.transform(matrix);
        }
        if let Some(c) = clip {
            self.clip_to(c);
        }
        self.content.x_object(Name(name.as_bytes()));
        self.content.restore_state();
        Ok(())
    }

    /// `x y w h re W n`: everything up to the matching `Q` is clipped to `rect`.
    fn clip_to(&mut self, rect: Rect) {
        self.content
            .rect(n(rect.x0), n(rect.y0), n(rect.width()), n(rect.height()));
        self.content.clip_nonzero();
        self.content.end_path();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shapes::{ellipse_path, polyline_path, rect_path};
    use crate::style::{Dash, Fill, ShapeStyle};
    use kurbo::Point;

    fn text(a: &Appearance) -> String {
        String::from_utf8_lossy(&a.content).into_owned()
    }

    fn ops(a: &Appearance) -> Vec<String> {
        text(a).lines().map(str::to_owned).collect()
    }

    #[test]
    fn empty_list_is_an_empty_stream() {
        let a = emit(&DrawList::new()).unwrap();
        assert!(a.content.is_empty());
        assert_eq!(a.bbox, Rect::ZERO);
        assert_eq!(a.matrix, Affine::IDENTITY);
        assert!(a.resources.is_empty());
    }

    #[test]
    fn an_opaque_stroked_rectangle() {
        let mut list = DrawList::new();
        list.push(DrawOp::Path(PathOp::stroked(
            rect_path(Rect::new(10.0, 20.0, 110.0, 70.0)),
            Stroke::new(2.0, Rgb::RED),
        )));
        let a = emit(&list).unwrap();
        assert_eq!(
            ops(&a),
            [
                "q", "1 0 0 RG", "2 w", "0 J", "0 j", "10 M", "10 20 m", "110 20 l", "110 70 l",
                "10 70 l", "h", "S", "Q"
            ]
        );
        assert!(a.resources.is_empty(), "opaque Normal needs no ExtGState");
        assert_eq!(a.bbox, Rect::new(9.0, 19.0, 111.0, 71.0));
    }

    #[test]
    fn opaque_fill_and_stroke_use_one_operator() {
        let mut list = DrawList::new();
        list.push(DrawOp::Path(
            PathOp::stroked(
                rect_path(Rect::new(0.0, 0.0, 10.0, 10.0)),
                Stroke::new(1.0, Rgb::RED),
            )
            .with_fill(Fill::new(Rgb::BLUE)),
        ));
        let o = ops(&emit(&list).unwrap());
        assert!(
            o.contains(&"0 0 1 rg".to_owned()) && o.contains(&"B".to_owned()),
            "{o:?}"
        );
        let mut list = DrawList::new();
        list.push(DrawOp::Path(
            PathOp::stroked(
                rect_path(Rect::new(0.0, 0.0, 10.0, 10.0)),
                Stroke::new(1.0, Rgb::RED),
            )
            .with_fill(Fill::even_odd(Rgb::BLUE)),
        ));
        assert!(ops(&emit(&list).unwrap()).contains(&"B*".to_owned()));
    }

    #[test]
    fn fill_rules_caps_joins_and_dashes_are_written() {
        let mut list = DrawList::new();
        list.push(DrawOp::Path(PathOp::filled(
            rect_path(Rect::new(0.0, 0.0, 10.0, 10.0)),
            Fill::even_odd(Rgb::GREEN),
        )));
        list.push(DrawOp::Path(PathOp::stroked(
            polyline_path(&[Point::ZERO, Point::new(10.0, 0.0)]),
            Stroke::new(1.5, Rgb::BLACK)
                .with_cap(LineCap::Round)
                .with_join(LineJoin::Bevel)
                .with_miter_limit(4.0)
                .with_dash(Dash::new([3.0, 1.5], 0.5)),
        )));
        let o = ops(&emit(&list).unwrap());
        for needed in [
            "0 1 0 rg",
            "f*",
            "1.5 w",
            "1 J",
            "2 j",
            "4 M",
            "[3 1.5] 0.5 d",
            "S",
        ] {
            assert!(
                o.contains(&needed.to_owned()),
                "missing {needed:?} in {o:?}"
            );
        }
    }

    #[test]
    fn an_odd_dash_array_is_written_out_in_full_and_an_invalid_one_is_solid() {
        let line = polyline_path(&[Point::ZERO, Point::new(10.0, 0.0)]);
        let mut list = DrawList::new();
        list.push(DrawOp::Path(PathOp::stroked(
            line.clone(),
            Stroke::new(1.0, Rgb::BLACK).with_dash(Dash::new([3.0], 0.0)),
        )));
        assert!(ops(&emit(&list).unwrap()).contains(&"[3 3] 0 d".to_owned()));
        let mut list = DrawList::new();
        list.push(DrawOp::Path(PathOp::stroked(
            line,
            Stroke::new(1.0, Rgb::BLACK).with_dash(Dash::new([0.0, 0.0], 0.0)),
        )));
        assert!(!text(&emit(&list).unwrap()).contains(" d\n"));
    }

    #[test]
    fn opacity_and_blend_become_one_shared_ext_gstate() {
        let mut list = DrawList::new();
        for _ in 0..2 {
            list.push(DrawOp::Path(
                PathOp::filled(
                    rect_path(Rect::new(0.0, 0.0, 10.0, 10.0)),
                    Fill::new(Rgb::YELLOW),
                )
                .with_opacity(0.4)
                .with_blend(BlendMode::Multiply),
            ));
        }
        list.push(DrawOp::Path(
            PathOp::filled(
                rect_path(Rect::new(0.0, 0.0, 10.0, 10.0)),
                Fill::new(Rgb::YELLOW),
            )
            .with_opacity(0.4),
        ));
        let a = emit(&list).unwrap();
        assert_eq!(
            a.resources.ext_gstates,
            [
                ExtGState {
                    name: "PlGs0".into(),
                    stroke_alpha: 0.4,
                    fill_alpha: 0.4,
                    blend: BlendMode::Multiply
                },
                ExtGState {
                    name: "PlGs1".into(),
                    stroke_alpha: 0.4,
                    fill_alpha: 0.4,
                    blend: BlendMode::Normal
                },
            ]
        );
        let o = ops(&a);
        assert_eq!(o.iter().filter(|l| *l == "/PlGs0 gs").count(), 2);
        assert_eq!(o.iter().filter(|l| *l == "/PlGs1 gs").count(), 1);
    }

    #[test]
    fn translucent_fill_and_stroke_are_painted_as_two_objects() {
        let mut list = DrawList::new();
        list.push(DrawOp::Path(
            PathOp::stroked(
                rect_path(Rect::new(0.0, 0.0, 10.0, 10.0)),
                Stroke::new(1.0, Rgb::RED),
            )
            .with_fill(Fill::new(Rgb::BLUE))
            .with_opacity(0.5),
        ));
        let o = ops(&emit(&list).unwrap());
        assert!(!o.contains(&"B".to_owned()));
        let f = o.iter().position(|l| l == "f").unwrap();
        let s = o.iter().position(|l| l == "S").unwrap();
        assert!(f < s, "fill first, then stroke: {o:?}");
        assert_eq!(
            o.iter().filter(|l| *l == "h").count(),
            2,
            "the path is built twice"
        );
    }

    #[test]
    fn a_translucent_shape_becomes_an_isolated_group_form() {
        let mut list = DrawList::new();
        list.push_shape(
            ellipse_path(Rect::new(10.0, 10.0, 50.0, 30.0)),
            &ShapeStyle::stroked(Stroke::new(2.0, Rgb::RED))
                .with_fill(Fill::new(Rgb::BLUE))
                .with_opacity(0.5),
        );
        let a = emit(&list).unwrap();
        assert_eq!(ops(&a), ["q", "/PlGs0 gs", "/PlX0 Do", "Q"]);
        assert_eq!(a.resources.ext_gstates[0].fill_alpha, 0.5);
        let [XObjectResource {
            name,
            object: XObject::Form(form),
        }] = &a.resources.xobjects[..]
        else {
            panic!("expected one form, got {:?}", a.resources.xobjects);
        };
        assert_eq!(name, "PlX0");
        assert_eq!(
            form.group,
            Some(TransparencyGroup {
                isolated: true,
                knockout: false
            })
        );
        // Inside the group the shape is opaque and uses the single operator.
        assert!(ops(&form.form).contains(&"B".to_owned()));
        assert!(form.form.resources.is_empty());
        // The group's box covers the stroke, and so does the appearance's.
        assert!((form.form.bbox.x0 - 9.0).abs() < 0.01 && (form.form.bbox.y1 - 31.0).abs() < 0.01);
        assert_eq!(a.bbox, form.form.bbox);
    }

    #[test]
    fn a_clipped_group_sets_a_clipping_path_inside_its_own_state() {
        let inner = DrawOp::Path(PathOp::filled(
            rect_path(Rect::new(0.0, 0.0, 40.0, 20.0)),
            Fill::new(Rgb::BLACK),
        ));
        let mut list = DrawList::new();
        list.push(DrawOp::Group(
            Group::new(vec![inner.clone()]).with_clip(Rect::new(5.0, 5.0, 15.0, 10.0)),
        ));
        let a = emit(&list).unwrap();
        let o = ops(&a);
        assert_eq!(o[..4], ["q", "5 5 10 5 re", "W", "n"], "{o:?}");
        assert_eq!(o.last().unwrap(), "Q");
        assert_eq!(a.bbox, Rect::new(5.0, 5.0, 15.0, 10.0), "only what shows");
        // Under a transform the clip is in the group's own coordinates: after the matrix.
        let mut list = DrawList::new();
        list.push(DrawOp::Group(
            Group::new(vec![inner])
                .with_transform(Affine::translate((30.0, 40.0)))
                .with_clip(Rect::new(5.0, 5.0, 15.0, 10.0)),
        ));
        let o = ops(&emit(&list).unwrap());
        assert_eq!(
            o[..5],
            ["q", "1 0 0 1 30 40 cm", "5 5 10 5 re", "W", "n"],
            "{o:?}"
        );
    }

    #[test]
    fn a_plain_group_is_inline_under_its_matrix() {
        let inner = DrawOp::Path(PathOp::filled(
            rect_path(Rect::new(0.0, 0.0, 4.0, 2.0)),
            Fill::new(Rgb::BLACK),
        ));
        let mut list = DrawList::new();
        list.push(DrawOp::Group(
            Group::new(vec![inner.clone()]).with_transform(Affine::translate((30.0, 40.0))),
        ));
        let a = emit(&list).unwrap();
        let o = ops(&a);
        assert_eq!(o[..2], ["q", "1 0 0 1 30 40 cm"]);
        assert_eq!(o.last().unwrap(), "Q");
        assert!(a.resources.xobjects.is_empty());
        assert_eq!(a.bbox, Rect::new(30.0, 40.0, 34.0, 42.0));

        // An identity, opaque group adds nothing at all.
        let mut flat = DrawList::new();
        flat.push(DrawOp::Group(Group::new(vec![inner.clone()])));
        let mut bare = DrawList::new();
        bare.push(inner);
        assert_eq!(emit(&flat).unwrap().content, emit(&bare).unwrap().content);
    }

    #[test]
    fn text_names_a_standard_font_and_escapes_the_string() {
        let mut list = DrawList::new();
        list.push(DrawOp::Text(
            TextRun::new(
                "A(b)\\ \u{e9}\u{20ac}",
                Point::new(72.0, 700.0),
                12.0,
                FontStyle::Bold,
                Rgb::BLACK,
            )
            .unwrap(),
        ));
        list.push(DrawOp::Text(
            TextRun::new("x", Point::new(72.0, 680.0), 9.0, FontStyle::Bold, Rgb::RED)
                .unwrap()
                .with_opacity(0.5),
        ));
        list.push(DrawOp::Text(
            TextRun::new(
                "y",
                Point::new(72.0, 660.0),
                9.0,
                FontStyle::Italic,
                Rgb::RED,
            )
            .unwrap(),
        ));
        let a = emit(&list).unwrap();
        assert_eq!(
            a.resources.fonts,
            [
                FontResource {
                    name: "PlF0".into(),
                    style: FontStyle::Bold,
                    base_font: "Helvetica-Bold",
                    encoding: "WinAnsiEncoding"
                },
                FontResource {
                    name: "PlF1".into(),
                    style: FontStyle::Italic,
                    base_font: "Helvetica-Oblique",
                    encoding: "WinAnsiEncoding"
                },
            ]
        );
        let o = ops(&a);
        assert!(o.contains(&"/PlF0 12 Tf".to_owned()), "{o:?}");
        assert!(o.contains(&"1 0 0 1 72 700 Tm".to_owned()), "{o:?}");
        assert!(o.contains(&"/PlF1 9 Tf".to_owned()), "{o:?}");
        assert_eq!(o.iter().filter(|l| *l == "BT").count(), 3);
        assert_eq!(a.resources.ext_gstates.len(), 1);
        // The string operand, whatever escaping the writer chose, decodes to the WinAnsi bytes.
        let shown = a
            .content
            .split(|b| *b == b'\n')
            .find(|l| l.ends_with(b" Tj"))
            .unwrap();
        assert_eq!(decode_string(&shown[..shown.len() - 3]), b"A(b)\\ \xE9\x80");
    }

    /// Decodes a PDF literal or hex string token.
    fn decode_string(token: &[u8]) -> Vec<u8> {
        if token.first() == Some(&b'<') {
            let hex: Vec<u8> = token[1..token.len() - 1]
                .iter()
                .filter(|b| b.is_ascii_hexdigit())
                .copied()
                .collect();
            return hex
                .chunks(2)
                .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
                .collect();
        }
        let body = &token[1..token.len() - 1];
        let mut out = Vec::new();
        let mut i = 0;
        while i < body.len() {
            if body[i] == b'\\' {
                i += 1;
                match body[i] {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    d if d.is_ascii_digit() => {
                        let mut v = 0u32;
                        let mut k = 0;
                        while k < 3 && i < body.len() && body[i].is_ascii_digit() {
                            v = v * 8 + (body[i] - b'0') as u32;
                            i += 1;
                            k += 1;
                        }
                        out.push(v as u8);
                        continue;
                    }
                    other => out.push(other),
                }
            } else {
                out.push(body[i]);
            }
            i += 1;
        }
        out
    }

    #[test]
    fn text_outside_winansi_is_an_error_not_a_dropped_character() {
        let mut list = DrawList::new();
        list.push(DrawOp::Text(TextRun {
            text: "ok \u{4e2d}".into(),
            origin: Point::ZERO,
            size: 10.0,
            style: FontStyle::Regular,
            color: Rgb::BLACK,
            opacity: 1.0,
            blend: BlendMode::Normal,
        }));
        assert_eq!(
            emit(&list),
            Err(DrawError::NotWinAnsi {
                ch: '\u{4e2d}',
                index: 3
            })
        );
    }

    #[test]
    fn an_image_is_stored_once_and_split_into_colour_and_alpha() {
        let image = Arc::new(RgbaImage::new(2, 1, vec![10, 20, 30, 255, 40, 50, 60, 128]).unwrap());
        let mut list = DrawList::new();
        list.push(DrawOp::Image(ImageOp::new(
            image.clone(),
            Rect::new(10.0, 20.0, 50.0, 40.0),
        )));
        list.push(DrawOp::Image(ImageOp::new(
            image,
            Rect::new(60.0, 20.0, 100.0, 40.0),
        )));
        let a = emit(&list).unwrap();
        assert_eq!(
            a.resources.xobjects,
            [XObjectResource {
                name: "PlX0".into(),
                object: XObject::Image(ImageXObject {
                    width: 2,
                    height: 1,
                    rgb: vec![10, 20, 30, 40, 50, 60],
                    alpha: Some(vec![255, 128]),
                })
            }]
        );
        let o = ops(&a);
        assert!(o.contains(&"40 0 0 20 10 20 cm".to_owned()), "{o:?}");
        assert!(o.contains(&"40 0 0 20 60 20 cm".to_owned()), "{o:?}");
        assert_eq!(o.iter().filter(|l| *l == "/PlX0 Do").count(), 2);

        let opaque = Arc::new(RgbaImage::new(1, 1, vec![1, 2, 3, 255]).unwrap());
        let mut list = DrawList::new();
        list.push(DrawOp::Image(ImageOp::new(
            opaque,
            Rect::new(0.0, 0.0, 1.0, 1.0),
        )));
        let a = emit(&list).unwrap();
        assert!(matches!(
            &a.resources.xobjects[0].object,
            XObject::Image(ImageXObject { alpha: None, .. })
        ));
    }

    #[test]
    fn quadratic_segments_are_raised_to_cubics() {
        let mut path = BezPath::new();
        path.move_to((0.0, 0.0));
        path.quad_to((30.0, 60.0), (60.0, 0.0));
        let mut list = DrawList::new();
        list.push(DrawOp::Path(PathOp::stroked(
            path,
            Stroke::new(1.0, Rgb::BLACK),
        )));
        let o = ops(&emit(&list).unwrap());
        assert!(o.contains(&"20 40 40 40 60 0 c".to_owned()), "{o:?}");
    }

    #[test]
    fn the_callers_bbox_is_used_and_invisible_ops_are_skipped() {
        let mut list = DrawList::new();
        list.push(DrawOp::Path(
            PathOp::filled(
                rect_path(Rect::new(0.0, 0.0, 10.0, 10.0)),
                Fill::new(Rgb::RED),
            )
            .with_opacity(0.0),
        ));
        list.push(DrawOp::Group(Group::new(vec![]).with_opacity(0.5)));
        let a = emit_with_bbox(&list, Rect::new(50.0, 60.0, 5.0, 6.0)).unwrap();
        assert!(a.content.is_empty());
        assert!(a.resources.is_empty());
        assert_eq!(a.bbox, Rect::new(5.0, 6.0, 50.0, 60.0));
        assert_eq!(
            emit_with_bbox(&list, Rect::new(0.0, 0.0, f64::NAN, 1.0)),
            Err(DrawError::InvalidNumber)
        );
    }

    #[test]
    fn save_and_restore_are_balanced() {
        let mut list = DrawList::new();
        list.push_shape(
            rect_path(Rect::new(0.0, 0.0, 10.0, 10.0)),
            &ShapeStyle::stroked(Stroke::new(1.0, Rgb::RED))
                .with_fill(Fill::new(Rgb::BLUE))
                .with_opacity(0.3)
                .with_blend(BlendMode::Multiply),
        );
        list.push(DrawOp::Group(
            Group::new(list.ops.clone()).with_transform(Affine::rotate(0.3)),
        ));
        fn check(a: &Appearance) {
            let o: Vec<String> = String::from_utf8_lossy(&a.content)
                .lines()
                .map(str::to_owned)
                .collect();
            let mut depth = 0i32;
            for l in &o {
                match l.as_str() {
                    "q" => depth += 1,
                    "Q" => {
                        depth -= 1;
                        assert!(depth >= 0);
                    }
                    _ => {}
                }
            }
            assert_eq!(depth, 0);
            for x in &a.resources.xobjects {
                if let XObject::Form(f) = &x.object {
                    check(&f.form);
                }
            }
        }
        check(&emit(&list).unwrap());
    }
}
