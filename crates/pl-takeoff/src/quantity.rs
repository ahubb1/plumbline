//! Quantity columns: what a measurement amounts to, shown in the list's units.
//!
//! A measurement's quantities are worked out by pl-annot in SI units from its geometry and its
//! scale ([`pl_annot::Annotation::quantities`]; the page's `/UserUnit` is part of the scale).
//! The list keeps those SI values ([`RowMeasure`]) and shows each quantity column in one unit
//! for the whole list ([`QuantityUnits`]), so that a column adds up. The cells hold the number
//! in that unit at full precision; the unit's decimal places only govern how it is written.

use pl_annot::{Annotation, MeasureKind, Quantities};
use pl_geom::{format_length, AreaUnit, LengthFormat, Precision, Unit, VolumeUnit};
use serde::{Deserialize, Serialize};

/// The measured values of a row, in SI units, as pl-annot gives them.
#[derive(Debug, Clone, PartialEq)]
pub struct RowMeasure {
    pub kind: MeasureKind,
    /// Metres, square metres, cubic metres, radians, a count.
    pub quantities: Quantities,
    /// The depth of a Volume, in metres.
    pub depth: Option<f64>,
    /// The scale as its `/Measure` states it (`/R`), empty without one.
    pub scale: String,
    /// The marker of a Count (`Dot`, `Ring`, ...).
    pub symbol: Option<String>,
}

impl RowMeasure {
    /// The measurement side of an annotation; `None` for one that is not a measurement.
    pub fn of(a: &Annotation) -> Option<RowMeasure> {
        let m = a.measurement()?;
        let quantities = a.quantities()?;
        Some(RowMeasure {
            kind: m.kind,
            quantities,
            depth: m.depth,
            scale: m.spec.as_ref().map(|s| s.ratio.clone()).unwrap_or_default(),
            symbol: (m.kind == MeasureKind::Count).then(|| m.symbol.name().to_string()),
        })
    }

    /// The real length the kind measures along a line or a run: the length of a Length or
    /// Polylength. `None` for the other kinds; a Diameter is in [`RowMeasure::diameter`], so
    /// that the Length total holds only runs.
    pub fn length(&self) -> Option<f64> {
        match self.kind {
            MeasureKind::Length | MeasureKind::Polylength => self.quantities.length,
            _ => None,
        }
    }

    /// The diameter of a Diameter, twice the radius of a Radius.
    pub fn diameter(&self) -> Option<f64> {
        match self.kind {
            MeasureKind::Diameter => self.quantities.length,
            MeasureKind::Radius => self.quantities.length.map(|r| r * 2.0),
            _ => None,
        }
    }

    /// The area of an Area (cutouts taken off). `None` for a Volume, whose area is its
    /// [`RowMeasure::base_area`], and for the other kinds.
    pub fn area(&self) -> Option<f64> {
        match self.kind {
            MeasureKind::Volume => None,
            _ => self.quantities.area,
        }
    }

    /// The base (plan) area of a Volume, cutouts taken off.
    pub fn base_area(&self) -> Option<f64> {
        match self.kind {
            MeasureKind::Volume => self.quantities.area,
            _ => None,
        }
    }

    /// The perimeter of a Perimeter. `None` for the other kinds: the outline of an Area or a
    /// Volume is not in the Perimeter column, so that its total holds only perimeters (as the
    /// Length total holds only runs); see [`RowMeasure::outline`].
    pub fn perimeter(&self) -> Option<f64> {
        match self.kind {
            MeasureKind::Perimeter => self.quantities.length,
            _ => None,
        }
    }

    /// The length around a closed measurement: a Perimeter, or the outer ring of an Area or a
    /// Volume. What Wall Area runs along when the row has no length.
    pub fn outline(&self) -> Option<f64> {
        match self.kind {
            MeasureKind::Perimeter | MeasureKind::Area | MeasureKind::Volume => {
                self.quantities.length
            }
            _ => None,
        }
    }

    /// The radius of a Radius, half the diameter of a Diameter.
    pub fn radius(&self) -> Option<f64> {
        match self.kind {
            MeasureKind::Radius => self.quantities.length,
            MeasureKind::Diameter => self.quantities.length.map(|d| d / 2.0),
            _ => None,
        }
    }
}

/// The places a number is written with, capped.
const MAX_PLACES: u8 = 12;

/// A fixed-point decimal without a "-0.00".
pub(crate) fn fixed(value: f64, places: u8) -> String {
    let places = places.min(MAX_PLACES) as usize;
    let s = format!("{value:.places$}");
    match s.strip_prefix('-') {
        Some(rest) if rest.chars().all(|c| c == '0' || c == '.') => rest.to_string(),
        _ => s,
    }
}

/// How the quantity columns of the list are shown: one unit and number of decimal places per
/// kind of quantity. Serialisable (units by their abbreviation).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "UnitsRepr", into = "UnitsRepr")]
pub struct QuantityUnits {
    /// Length, Perimeter, Radius and Depth. [`Unit::FeetInches`] is a number of feet, written
    /// as feet and inches.
    pub length: Unit,
    pub length_places: u8,
    /// Area and Wall Area.
    pub area: AreaUnit,
    pub area_places: u8,
    pub volume: VolumeUnit,
    pub volume_places: u8,
    /// Angles are in degrees.
    pub angle_places: u8,
}

#[derive(Serialize, Deserialize)]
struct UnitsRepr {
    length: String,
    length_places: u8,
    area: String,
    area_places: u8,
    volume: String,
    volume_places: u8,
    angle_places: u8,
}

impl From<QuantityUnits> for UnitsRepr {
    fn from(u: QuantityUnits) -> UnitsRepr {
        UnitsRepr {
            length: u.length.abbreviation().to_string(),
            length_places: u.length_places,
            area: u.area.abbreviation().to_string(),
            area_places: u.area_places,
            volume: u.volume.abbreviation().to_string(),
            volume_places: u.volume_places,
            angle_places: u.angle_places,
        }
    }
}

impl TryFrom<UnitsRepr> for QuantityUnits {
    type Error = String;

    fn try_from(r: UnitsRepr) -> Result<QuantityUnits, String> {
        Ok(QuantityUnits {
            length: length_unit(&r.length).ok_or_else(|| format!("unknown unit {}", r.length))?,
            length_places: r.length_places.min(MAX_PLACES),
            area: AreaUnit::from_abbreviation(&r.area)
                .ok_or_else(|| format!("unknown unit {}", r.area))?,
            area_places: r.area_places.min(MAX_PLACES),
            volume: VolumeUnit::from_abbreviation(&r.volume)
                .ok_or_else(|| format!("unknown unit {}", r.volume))?,
            volume_places: r.volume_places.min(MAX_PLACES),
            angle_places: r.angle_places.min(MAX_PLACES),
        })
    }
}

/// The length unit with this abbreviation (`ft`, `m`, ...).
pub fn length_unit(label: &str) -> Option<Unit> {
    Unit::ALL
        .into_iter()
        .find(|u| u.abbreviation() == label.trim())
}

impl Default for QuantityUnits {
    fn default() -> QuantityUnits {
        QuantityUnits::metric()
    }
}

/// Which quantity a column holds, for its unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QuantityKind {
    Length,
    Area,
    Volume,
    Angle,
    Count,
}

impl QuantityUnits {
    /// Feet, square feet and cubic yards, two places.
    pub fn imperial() -> QuantityUnits {
        QuantityUnits {
            length: Unit::Foot,
            length_places: 2,
            area: AreaUnit::SqFoot,
            area_places: 2,
            volume: VolumeUnit::CuYard,
            volume_places: 2,
            angle_places: 1,
        }
    }

    /// Metres, square metres and cubic metres, two places.
    pub fn metric() -> QuantityUnits {
        QuantityUnits {
            length: Unit::Metre,
            length_places: 2,
            area: AreaUnit::SqMetre,
            area_places: 2,
            volume: VolumeUnit::CuMetre,
            volume_places: 2,
            angle_places: 1,
        }
    }

    /// Imperial when more of the measurements among `annotations` are on an imperial scale
    /// than on a metric one, metric otherwise (also when there are none).
    pub fn for_annotations<'a>(
        annotations: impl IntoIterator<Item = &'a Annotation>,
    ) -> QuantityUnits {
        let (mut imperial, mut metric) = (0usize, 0usize);
        for a in annotations {
            if let Some(spec) = a.measurement().and_then(|m| m.spec.as_ref()) {
                if spec.metres_per_unit().is_none() {
                    continue;
                }
                if spec.is_imperial() {
                    imperial += 1;
                } else {
                    metric += 1;
                }
            }
        }
        if imperial > metric {
            QuantityUnits::imperial()
        } else {
            QuantityUnits::metric()
        }
    }

    /// Metres in the length unit.
    pub fn length(&self, metres: f64) -> f64 {
        self.length.from_metres(metres)
    }

    /// A number in the length unit, in metres.
    pub fn length_to_metres(&self, value: f64) -> f64 {
        self.length.to_metres(value)
    }

    pub fn area(&self, sq_metres: f64) -> f64 {
        self.area.from_sq_metres(sq_metres)
    }

    pub fn volume(&self, cu_metres: f64) -> f64 {
        self.volume.from_cu_metres(cu_metres)
    }

    /// The label of the unit a kind of quantity is shown in.
    pub fn label(&self, kind: QuantityKind) -> &'static str {
        match kind {
            QuantityKind::Length => match self.length {
                Unit::FeetInches => "ft",
                u => u.abbreviation(),
            },
            QuantityKind::Area => self.area.abbreviation(),
            QuantityKind::Volume => self.volume.abbreviation(),
            QuantityKind::Angle => "\u{b0}",
            QuantityKind::Count => "ea",
        }
    }

    /// The decimal places a kind of quantity is written with.
    pub fn places(&self, kind: QuantityKind) -> u8 {
        match kind {
            QuantityKind::Length => self.length_places,
            QuantityKind::Area => self.area_places,
            QuantityKind::Volume => self.volume_places,
            QuantityKind::Angle => self.angle_places,
            QuantityKind::Count => 0,
        }
    }

    /// A number of a kind of quantity, already in the list's unit, as text without the unit
    /// (feet and inches are written as such).
    pub fn number_text(&self, kind: QuantityKind, value: f64) -> String {
        if kind == QuantityKind::Length && self.length == Unit::FeetInches {
            return format_length(
                self.length_to_metres(value),
                &LengthFormat::new(Unit::FeetInches, Precision::Decimal(self.length_places)),
            );
        }
        fixed(value, self.places(kind))
    }

    /// A number of a kind of quantity, already in the list's unit, as text with its unit:
    /// `12.50 ft`, `3.25 sq m`, `45.0°`, `12 ea`.
    pub fn text(&self, kind: QuantityKind, value: f64) -> String {
        let number = self.number_text(kind, value);
        match kind {
            QuantityKind::Length if self.length == Unit::FeetInches => number,
            QuantityKind::Angle => format!("{number}\u{b0}"),
            _ => format!("{number} {}", self.label(kind)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_serialise_by_abbreviation() {
        let u = QuantityUnits::imperial();
        let json = serde_json::to_string(&u).unwrap();
        assert!(json.contains("\"cu yd\""), "{json}");
        assert_eq!(serde_json::from_str::<QuantityUnits>(&json).unwrap(), u);
        assert!(serde_json::from_str::<QuantityUnits>(&json.replace("cu yd", "pints")).is_err());
    }

    #[test]
    fn text_forms() {
        let mut u = QuantityUnits::imperial();
        assert_eq!(u.text(QuantityKind::Length, 12.5), "12.50 ft");
        assert_eq!(u.text(QuantityKind::Area, -0.001), "0.00 sq ft");
        assert_eq!(u.text(QuantityKind::Angle, 45.0), "45.0\u{b0}");
        assert_eq!(u.text(QuantityKind::Count, 3.0), "3 ea");
        u.length = Unit::FeetInches;
        u.length_places = 0;
        assert_eq!(u.text(QuantityKind::Length, 12.5), "12'-6\"");
    }
}
