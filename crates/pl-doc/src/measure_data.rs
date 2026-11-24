//! Measurements, scales and viewports as plain serialisable data: what the journal, the
//! clipboard and the command file carry for them.
//!
//! Every number is an `f64` written with exact round-tripping, so a value that goes through
//! JSON comes back bit for bit: `MeasureSpecData::from(&spec).to_spec() == spec`.
//!
//! [`ScaleSetting`] is how a command names a scale: the drawing scale itself (paper and real
//! lengths with their units) and how values are shown. It is turned into a page's
//! [`MeasureSpec`] with that page's `/UserUnit` only when the command is applied, so one
//! setting can be applied to pages of different user units.

use pl_annot::{
    CaptionPosition, CountSymbol, MeasureFormat, MeasureKind, MeasureSpec, Measurement,
    NumberFormat, NumberStyle, ScaleRef,
};
use pl_geom::{
    AreaFormat, AreaUnit, LengthFormat, Point, Precision, Rect, Scale, Unit, Vec2, VolumeFormat,
    VolumeUnit,
};
use serde::{Deserialize, Serialize};

fn pt(p: Point) -> [f64; 2] {
    [p.x, p.y]
}

fn point(p: [f64; 2]) -> Point {
    Point::new(p[0], p[1])
}

/// A length unit by its abbreviation (`pt`, `in`, `ft`, `ft-in`, `mm`, `cm`, `m`).
pub fn unit_from_name(name: &str) -> Option<Unit> {
    let key = name.trim().to_ascii_lowercase();
    Unit::ALL.into_iter().find(|u| u.abbreviation() == key)
}

/// [`NumberFormat`] as plain data. `style` is the PDF name (`D`, `F`, `R`, `T`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NumberFormatData {
    pub label: String,
    pub factor: f64,
    pub precision: u32,
    pub style: String,
    #[serde(default)]
    pub fixed_denominator: bool,
    #[serde(default)]
    pub thousands: String,
    #[serde(default)]
    pub decimal: String,
    #[serde(default)]
    pub prefix_space: String,
    #[serde(default)]
    pub suffix_space: String,
    #[serde(default)]
    pub label_first: bool,
}

impl From<&NumberFormat> for NumberFormatData {
    fn from(f: &NumberFormat) -> NumberFormatData {
        NumberFormatData {
            label: f.label.clone(),
            factor: f.factor,
            precision: f.precision,
            style: f.style.pdf_name().to_string(),
            fixed_denominator: f.fixed_denominator,
            thousands: f.thousands.clone(),
            decimal: f.decimal.clone(),
            prefix_space: f.prefix_space.clone(),
            suffix_space: f.suffix_space.clone(),
            label_first: f.label_first,
        }
    }
}

impl NumberFormatData {
    pub fn to_format(&self) -> NumberFormat {
        NumberFormat {
            label: self.label.clone(),
            factor: self.factor,
            precision: self.precision,
            style: NumberStyle::from_pdf_name(&self.style).unwrap_or_default(),
            fixed_denominator: self.fixed_denominator,
            thousands: self.thousands.clone(),
            decimal: self.decimal.clone(),
            prefix_space: self.prefix_space.clone(),
            suffix_space: self.suffix_space.clone(),
            label_first: self.label_first,
        }
    }
}

fn formats(v: &[NumberFormat]) -> Vec<NumberFormatData> {
    v.iter().map(NumberFormatData::from).collect()
}

fn to_formats(v: &[NumberFormatData]) -> Vec<NumberFormat> {
    v.iter().map(NumberFormatData::to_format).collect()
}

/// A PDF measure dictionary ([`MeasureSpec`]) as plain data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeasureSpecData {
    pub ratio: String,
    pub x: Vec<NumberFormatData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<Vec<NumberFormatData>>,
    pub distance: Vec<NumberFormatData>,
    pub area: Vec<NumberFormatData>,
    #[serde(default)]
    pub angle: Vec<NumberFormatData>,
    #[serde(default)]
    pub slope: Vec<NumberFormatData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cyx: Option<f64>,
    /// `/V`, Bluebeam Revu's volume formats.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volume: Vec<NumberFormatData>,
    /// Revu's `/TargetUnitConversion`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_unit_conversion: Option<f64>,
}

impl From<&MeasureSpec> for MeasureSpecData {
    fn from(s: &MeasureSpec) -> MeasureSpecData {
        MeasureSpecData {
            ratio: s.ratio.clone(),
            x: formats(&s.x),
            y: s.y.as_deref().map(formats),
            distance: formats(&s.distance),
            area: formats(&s.area),
            angle: formats(&s.angle),
            slope: formats(&s.slope),
            origin: s.origin.map(pt),
            cyx: s.cyx,
            volume: formats(&s.volume),
            target_unit_conversion: s.target_unit_conversion,
        }
    }
}

impl MeasureSpecData {
    pub fn to_spec(&self) -> MeasureSpec {
        MeasureSpec {
            ratio: self.ratio.clone(),
            x: to_formats(&self.x),
            y: self.y.as_deref().map(to_formats),
            distance: to_formats(&self.distance),
            area: to_formats(&self.area),
            angle: to_formats(&self.angle),
            slope: to_formats(&self.slope),
            origin: self.origin.map(point),
            cyx: self.cyx,
            volume: to_formats(&self.volume),
            target_unit_conversion: self.target_unit_conversion,
        }
    }
}

/// How a length is rounded: `{"decimal": 2}` or `{"fraction": 16}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrecisionData {
    Decimal(u8),
    Fraction(u32),
}

impl From<Precision> for PrecisionData {
    fn from(p: Precision) -> PrecisionData {
        match p {
            Precision::Decimal(places) => PrecisionData::Decimal(places),
            Precision::Fraction(den) => PrecisionData::Fraction(den),
        }
    }
}

impl From<PrecisionData> for Precision {
    fn from(p: PrecisionData) -> Precision {
        match p {
            PrecisionData::Decimal(places) => Precision::Decimal(places),
            PrecisionData::Fraction(den) => Precision::Fraction(den),
        }
    }
}

/// How measured values are shown ([`MeasureFormat`]) as plain data. Units are their
/// abbreviations as pl-geom writes them: `ft-in`, `ft`, `m`, ... for lengths, `sq ft`,
/// `sq m`, `acres`, ... for areas.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FormatData {
    pub length_unit: String,
    pub precision: PrecisionData,
    pub area_unit: String,
    pub area_places: u8,
    #[serde(default = "one_place")]
    pub angle_places: u8,
}

fn one_place() -> u8 {
    1
}

impl From<&MeasureFormat> for FormatData {
    fn from(f: &MeasureFormat) -> FormatData {
        FormatData {
            length_unit: f.length.unit.abbreviation().to_string(),
            precision: f.length.precision.into(),
            area_unit: f.area.unit.abbreviation().to_string(),
            area_places: f.area.places,
            angle_places: f.angle_places,
        }
    }
}

impl FormatData {
    pub fn to_format(&self) -> Result<MeasureFormat, String> {
        let unit = unit_from_name(&self.length_unit)
            .ok_or_else(|| format!("unknown length unit {:?}", self.length_unit))?;
        let area = AreaUnit::from_abbreviation(&self.area_unit)
            .ok_or_else(|| format!("unknown area unit {:?}", self.area_unit))?;
        let mut format = MeasureFormat::new(
            LengthFormat::new(unit, self.precision.into()),
            AreaFormat::new(area, self.area_places),
        );
        format.angle_places = self.angle_places;
        Ok(format)
    }
}

/// A drawing scale and how values taken at it are shown, as a command gives it.
///
/// The scale is kept as its two sides, exactly as given; [`ScaleSetting::spec`] builds the
/// measure dictionary for a page with a given `/UserUnit`. Without `format`, values are shown
/// the usual way for the scale's real unit ([`MeasureFormat::for_scale`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScaleSetting {
    pub paper: f64,
    pub paper_unit: String,
    pub real: f64,
    pub real_unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<FormatData>,
}

impl ScaleSetting {
    pub fn new(scale: &Scale, format: Option<&MeasureFormat>) -> ScaleSetting {
        ScaleSetting {
            paper: scale.paper(),
            paper_unit: scale.paper_unit().abbreviation().to_string(),
            real: scale.real(),
            real_unit: scale.real_unit().abbreviation().to_string(),
            format: format.map(FormatData::from),
        }
    }

    /// Reads a scale as a person writes it (`1" = 20'`, `1:100`, `1/4" = 1'-0"`; see
    /// [`Scale::parse`]).
    pub fn parse(text: &str, format: Option<&MeasureFormat>) -> Result<ScaleSetting, String> {
        let scale = Scale::parse(text).map_err(|e| format!("{text:?} is not a scale: {e}"))?;
        Ok(ScaleSetting::new(&scale, format))
    }

    pub fn scale(&self) -> Result<Scale, String> {
        let unit = |name: &str| {
            unit_from_name(name).ok_or_else(|| format!("unknown length unit {name:?}"))
        };
        Scale::new(
            self.paper,
            unit(&self.paper_unit)?,
            self.real,
            unit(&self.real_unit)?,
        )
        .map_err(|e| e.to_string())
    }

    pub fn format(&self) -> Result<MeasureFormat, String> {
        match &self.format {
            Some(f) => f.to_format(),
            None => Ok(MeasureFormat::for_scale(&self.scale()?)),
        }
    }

    /// The measure dictionary of this scale on a page with `/UserUnit` `user_unit`.
    pub fn spec(&self, user_unit: f64) -> Result<MeasureSpec, String> {
        Ok(MeasureSpec::new(&self.scale()?, user_unit, &self.format()?))
    }
}

/// One viewport of a page as plain data: its name, box (`[x0, y0, x1, y1]`, unrotated page
/// space) and scale.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewportData {
    pub name: String,
    pub bbox: [f64; 4],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<MeasureSpecData>,
}

impl ViewportData {
    pub fn rect(&self) -> Rect {
        Rect::new(self.bbox[0], self.bbox[1], self.bbox[2], self.bbox[3]).abs()
    }
}

/// The scales of one page as plain data: the page's own and its viewports in `/VP` order.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct PageScalesData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_scale: Option<MeasureSpecData>,
    #[serde(default)]
    pub viewports: Vec<ViewportData>,
}

/// The measurement side of an annotation ([`Measurement`]) as plain data. `viewport` names
/// the viewport its scale comes from; absent, the page's own scale.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeasurementData {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<MeasureSpecData>,
    pub leader: f64,
    pub leader_extension: f64,
    pub leader_offset: f64,
    pub caption: bool,
    /// `Inline` or `Top`.
    pub caption_position: String,
    pub caption_offset: [f64; 2],
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cutouts: Vec<Vec<[f64; 2]>>,
    /// Metres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<f64>,
    /// The volume unit's abbreviation.
    pub volume_unit: String,
    pub volume_places: u8,
    pub symbol: String,
    pub symbol_size: f64,
    pub text_size: f64,
    pub rotation: u16,
}

impl From<&Measurement> for MeasurementData {
    fn from(m: &Measurement) -> MeasurementData {
        MeasurementData {
            kind: m.kind.name().to_string(),
            viewport: match &m.scale {
                ScaleRef::Page => None,
                ScaleRef::Viewport(name) => Some(name.clone()),
            },
            spec: m.spec.as_ref().map(MeasureSpecData::from),
            leader: m.leader,
            leader_extension: m.leader_extension,
            leader_offset: m.leader_offset,
            caption: m.caption,
            caption_position: m.caption_position.pdf_name().to_string(),
            caption_offset: [m.caption_offset.x, m.caption_offset.y],
            cutouts: m
                .cutouts
                .iter()
                .map(|r| r.iter().map(|p| pt(*p)).collect())
                .collect(),
            depth: m.depth,
            volume_unit: m.volume.unit.abbreviation().to_string(),
            volume_places: m.volume.places,
            symbol: m.symbol.name().to_string(),
            symbol_size: m.symbol_size,
            text_size: m.text_size,
            rotation: m.rotation,
        }
    }
}

impl MeasurementData {
    /// The measurement this describes; `None` when its kind is not one this version knows.
    pub fn to_measurement(&self) -> Option<Measurement> {
        let kind = MeasureKind::from_name(&self.kind)?;
        let mut m = Measurement::new(kind, self.spec.as_ref().map(MeasureSpecData::to_spec));
        m.scale = match &self.viewport {
            Some(name) => ScaleRef::Viewport(name.clone()),
            None => ScaleRef::Page,
        };
        m.leader = self.leader;
        m.leader_extension = self.leader_extension;
        m.leader_offset = self.leader_offset;
        m.caption = self.caption;
        m.caption_position =
            CaptionPosition::from_pdf_name(&self.caption_position).unwrap_or_default();
        m.caption_offset = Vec2::new(self.caption_offset[0], self.caption_offset[1]);
        m.cutouts = self
            .cutouts
            .iter()
            .map(|r| r.iter().map(|p| point(*p)).collect())
            .collect();
        m.depth = self.depth;
        if let Some(unit) = VolumeUnit::from_abbreviation(&self.volume_unit) {
            m.volume = VolumeFormat::new(unit, self.volume_places);
        }
        m.symbol = CountSymbol::from_name(&self.symbol).unwrap_or_default();
        m.symbol_size = self.symbol_size;
        m.text_size = self.text_size;
        m.rotation = self.rotation;
        Some(m)
    }
}

/// True when every number of the measurement is finite.
pub(crate) fn measurement_is_finite(m: &Measurement) -> bool {
    let fin = |v: f64| v.is_finite();
    fin(m.leader)
        && fin(m.leader_extension)
        && fin(m.leader_offset)
        && fin(m.caption_offset.x)
        && fin(m.caption_offset.y)
        && fin(m.symbol_size)
        && fin(m.text_size)
        && m.depth.is_none_or(fin)
        && m.cutouts
            .iter()
            .all(|r| r.iter().all(|p| fin(p.x) && fin(p.y)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eng(feet: f64, user_unit: f64) -> MeasureSpec {
        let scale = Scale::engineering(feet).unwrap();
        MeasureSpec::new(&scale, user_unit, &MeasureFormat::for_scale(&scale))
    }

    #[test]
    fn specs_and_measurements_survive_json_exactly() {
        let mut spec = MeasureSpec::with_axes(
            &Scale::parse("1:100").unwrap(),
            &Scale::parse("1:20").unwrap(),
            2.0,
            &MeasureFormat::for_scale(&Scale::parse("1:100").unwrap()),
        );
        spec.origin = Some(Point::new(0.1 + 0.2, -1.0 / 3.0));
        let data = MeasureSpecData::from(&spec);
        let json = serde_json::to_string(&data).unwrap();
        let back: MeasureSpecData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.to_spec(), spec);

        let mut m = Measurement::new(MeasureKind::Volume, Some(eng(20.0, 1.0)));
        m.scale = ScaleRef::Viewport("Detail A".into());
        m.cutouts = vec![vec![
            Point::new(1.0 / 3.0, 2.0),
            Point::new(5.0, 2.0),
            Point::new(5.0, 7.25),
        ]];
        m.depth = Some(0.15);
        m.leader = -12.5;
        m.caption_position = CaptionPosition::Inline;
        m.caption_offset = Vec2::new(3.0, -4.0);
        m.symbol = CountSymbol::Hexagon;
        m.volume = VolumeFormat::new(VolumeUnit::CuFoot, 3);
        m.rotation = 270;
        let data = MeasurementData::from(&m);
        let json = serde_json::to_string(&data).unwrap();
        let back: MeasurementData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.to_measurement().unwrap(), m);
    }

    #[test]
    fn a_scale_setting_builds_the_spec_for_each_user_unit() {
        let setting = ScaleSetting::parse("1\" = 20'", None).unwrap();
        assert_eq!(setting.spec(1.0).unwrap(), eng(20.0, 1.0));
        assert_eq!(setting.spec(2.0).unwrap(), eng(20.0, 2.0));
        // The /X factor converts page units to feet: one inch of paper (72 units) is 20 ft,
        // and a user unit of 2 doubles every page unit.
        let x1 = setting.spec(1.0).unwrap().x_factor();
        let x2 = setting.spec(2.0).unwrap().x_factor();
        assert!((x1 * 72.0 - 20.0).abs() < 1e-12, "{x1}");
        assert!((x2 / x1 - 2.0).abs() < 1e-12);

        let metric = MeasureFormat::new(
            LengthFormat::decimal(Unit::Metre, 3),
            AreaFormat::new(AreaUnit::SqMetre, 1),
        );
        let setting = ScaleSetting::parse("1:50", Some(&metric)).unwrap();
        let json = serde_json::to_string(&setting).unwrap();
        let back: ScaleSetting = serde_json::from_str(&json).unwrap();
        assert_eq!(back, setting);
        assert_eq!(back.format().unwrap(), metric);
        assert!(ScaleSetting::parse("twenty", None).is_err());
        let mut bad = setting.clone();
        bad.real_unit = "furlong".into();
        assert!(bad.spec(1.0).is_err());
    }
}
