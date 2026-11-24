//! Paint styles: colour, stroke, fill, dash, blend mode.

/// An opaque RGB colour (DeviceRGB), each component in `0.0..=1.0`. Transparency is never part of
/// a colour: it is the `opacity` of the op or group that uses it, as in PDF (`/C` and `/CA`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl Rgb {
    pub const BLACK: Rgb = Rgb::new(0.0, 0.0, 0.0);
    pub const WHITE: Rgb = Rgb::new(1.0, 1.0, 1.0);
    pub const RED: Rgb = Rgb::new(1.0, 0.0, 0.0);
    pub const GREEN: Rgb = Rgb::new(0.0, 1.0, 0.0);
    pub const BLUE: Rgb = Rgb::new(0.0, 0.0, 1.0);
    pub const YELLOW: Rgb = Rgb::new(1.0, 1.0, 0.0);

    pub const fn new(r: f32, g: f32, b: f32) -> Rgb {
        Rgb { r, g, b }
    }

    pub fn from_u8(r: u8, g: u8, b: u8) -> Rgb {
        Rgb::new(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
    }

    /// The colour as 8-bit components, rounded to nearest.
    pub fn to_u8(self) -> [u8; 3] {
        let c = self.clamped();
        let q = |v: f32| (v * 255.0).round() as u8;
        [q(c.r), q(c.g), q(c.b)]
    }

    /// Each component forced into `0.0..=1.0`; a NaN becomes 0.
    pub fn clamped(self) -> Rgb {
        let c = |v: f32| if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
        Rgb::new(c(self.r), c(self.g), c(self.b))
    }
}

/// How an op or group is combined with what is beneath it. The PDF names are the same.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BlendMode {
    #[default]
    Normal,
    /// Darkens: the result is backdrop times source, so dark ink under a highlight stays dark.
    Multiply,
}

impl BlendMode {
    /// The value of `/BM` in an ExtGState.
    pub fn pdf_name(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Multiply => "Multiply",
        }
    }
}

/// Which points are inside a filled path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FillRule {
    #[default]
    NonZero,
    EvenOdd,
}

/// The shape of the ends of an open stroked subpath and of each dash (PDF `J`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}

/// The shape of a stroked corner (PDF `j`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

/// A dash pattern as in PDF: lengths of alternating dashes and gaps in page units, and the
/// distance into the pattern at which the stroke starts. This is what `/BS /D` holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Dash {
    pub array: Vec<f64>,
    pub phase: f64,
}

impl Dash {
    /// The shortest dash that is drawn; shorter ones (zero included) are lengthened to this.
    pub const MIN_DASH: f64 = 0.01;

    pub fn new(array: impl Into<Vec<f64>>, phase: f64) -> Dash {
        Dash {
            array: array.into(),
            phase,
        }
    }

    /// Dashes and gaps of the same length.
    pub fn even(length: f64) -> Dash {
        Dash::new([length, length], 0.0)
    }

    /// The pattern as an even-length on/off list, or `None` when the stroke is solid: an empty
    /// array, a negative or non-finite entry, or all zeros (which PDF forbids). An odd-length
    /// array is repeated once, which is what PDF's cycling through the array amounts to.
    pub fn effective(&self) -> Option<(Vec<f64>, f64)> {
        if self.array.is_empty()
            || !self.phase.is_finite()
            || self.array.iter().any(|v| !v.is_finite() || *v < 0.0)
            || self.array.iter().sum::<f64>() <= 0.0
        {
            return None;
        }
        let mut array = self.array.clone();
        if array.len() % 2 == 1 {
            array.extend_from_slice(&self.array);
        }
        // A zero-length dash should be a dot under round or square caps, but renderers do not
        // agree on it (PDFium places such dots differently from Skia). A very short dash is
        // drawn the same everywhere and looks identical.
        for on in array.iter_mut().step_by(2) {
            if *on < Dash::MIN_DASH {
                *on = Dash::MIN_DASH;
            }
        }
        Some((array, self.phase))
    }

    /// A dotted line for round caps: dots `pitch` apart, each as wide as the line.
    pub fn dotted(pitch: f64) -> Dash {
        Dash::new([0.0, pitch], 0.0)
    }
}

/// How a path is stroked. Widths and dash lengths are in page units.
#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    /// Line width. Zero means the thinnest line the device can draw, as in PDF.
    pub width: f64,
    pub color: Rgb,
    pub dash: Option<Dash>,
    pub cap: LineCap,
    pub join: LineJoin,
    /// Miter joins longer than `miter_limit * width` are bevelled (PDF `M`).
    pub miter_limit: f64,
}

impl Stroke {
    /// PDF's default miter limit.
    pub const DEFAULT_MITER_LIMIT: f64 = 10.0;

    /// A solid stroke with butt caps and miter joins.
    pub fn new(width: f64, color: Rgb) -> Stroke {
        Stroke {
            width,
            color,
            dash: None,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter_limit: Stroke::DEFAULT_MITER_LIMIT,
        }
    }

    pub fn with_dash(mut self, dash: Dash) -> Stroke {
        self.dash = Some(dash);
        self
    }

    pub fn with_cap(mut self, cap: LineCap) -> Stroke {
        self.cap = cap;
        self
    }

    pub fn with_join(mut self, join: LineJoin) -> Stroke {
        self.join = join;
        self
    }

    pub fn with_miter_limit(mut self, limit: f64) -> Stroke {
        self.miter_limit = limit;
        self
    }

    /// The same stroke without its dash pattern.
    pub fn solid(&self) -> Stroke {
        Stroke {
            dash: None,
            ..self.clone()
        }
    }

    /// The dash pattern that actually applies, if any (see [`Dash::effective`]).
    pub fn effective_dash(&self) -> Option<(Vec<f64>, f64)> {
        self.dash.as_ref().and_then(Dash::effective)
    }
}

/// How a path is filled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fill {
    pub color: Rgb,
    pub rule: FillRule,
}

impl Fill {
    /// A non-zero fill.
    pub fn new(color: Rgb) -> Fill {
        Fill {
            color,
            rule: FillRule::NonZero,
        }
    }

    pub fn even_odd(color: Rgb) -> Fill {
        Fill {
            color,
            rule: FillRule::EvenOdd,
        }
    }
}

/// Stroke, fill, opacity and blend mode of one shape, as an annotation describes them (`/C`,
/// `/IC`, `/BS`, `/CA`). [`DrawList::push_shape`](crate::DrawList::push_shape) turns a path and
/// a style into ops.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapeStyle {
    pub stroke: Option<Stroke>,
    pub fill: Option<Fill>,
    pub opacity: f64,
    pub blend: BlendMode,
}

impl ShapeStyle {
    pub fn stroked(stroke: Stroke) -> ShapeStyle {
        ShapeStyle {
            stroke: Some(stroke),
            fill: None,
            opacity: 1.0,
            blend: BlendMode::Normal,
        }
    }

    pub fn filled(fill: Fill) -> ShapeStyle {
        ShapeStyle {
            stroke: None,
            fill: Some(fill),
            opacity: 1.0,
            blend: BlendMode::Normal,
        }
    }

    pub fn with_fill(mut self, fill: Fill) -> ShapeStyle {
        self.fill = Some(fill);
        self
    }

    pub fn with_stroke(mut self, stroke: Stroke) -> ShapeStyle {
        self.stroke = Some(stroke);
        self
    }

    pub fn with_opacity(mut self, opacity: f64) -> ShapeStyle {
        self.opacity = opacity;
        self
    }

    pub fn with_blend(mut self, blend: BlendMode) -> ShapeStyle {
        self.blend = blend;
        self
    }
}

/// Opacity forced into `0.0..=1.0`; a NaN counts as opaque.
pub(crate) fn clamp_opacity(opacity: f64) -> f64 {
    if opacity.is_nan() {
        1.0
    } else {
        opacity.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_round_trips_through_bytes() {
        let c = Rgb::from_u8(12, 200, 255);
        assert_eq!(c.to_u8(), [12, 200, 255]);
        assert_eq!(Rgb::new(-1.0, 2.0, f32::NAN).to_u8(), [0, 255, 0]);
    }

    #[test]
    fn dash_effective_handles_the_pdf_cases() {
        assert_eq!(
            Dash::new([3.0, 2.0], 1.0).effective(),
            Some((vec![3.0, 2.0], 1.0))
        );
        // Odd arrays cycle: [3] is 3 on, 3 off; [1 2 3] is 1 on 2 off 3 on 1 off 2 on 3 off.
        assert_eq!(
            Dash::new([3.0], 0.0).effective(),
            Some((vec![3.0, 3.0], 0.0))
        );
        assert_eq!(
            Dash::new([1.0, 2.0, 3.0], 0.0).effective().unwrap().0,
            vec![1.0, 2.0, 3.0, 1.0, 2.0, 3.0]
        );
        // Solid: empty, all zero, negative, non-finite.
        assert_eq!(Dash::new(Vec::<f64>::new(), 0.0).effective(), None);
        assert_eq!(Dash::new([0.0, 0.0], 0.0).effective(), None);
        assert_eq!(Dash::new([3.0, -1.0], 0.0).effective(), None);
        assert_eq!(Dash::new([3.0, f64::NAN], 0.0).effective(), None);
        // A zero-length dash (a dot) becomes a very short one; gaps may be zero.
        assert_eq!(
            Dash::dotted(7.0).effective(),
            Some((vec![Dash::MIN_DASH, 7.0], 0.0))
        );
        assert_eq!(
            Dash::new([4.0, 0.0], 0.0).effective(),
            Some((vec![4.0, 0.0], 0.0))
        );
        assert_eq!(Dash::new([3.0, 1.0], f64::INFINITY).effective(), None);
    }

    #[test]
    fn opacity_is_clamped() {
        assert_eq!(clamp_opacity(1.5), 1.0);
        assert_eq!(clamp_opacity(-0.5), 0.0);
        assert_eq!(clamp_opacity(f64::NAN), 1.0);
        assert_eq!(clamp_opacity(0.25), 0.25);
    }
}
