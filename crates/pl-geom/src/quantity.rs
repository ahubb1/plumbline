//! Area, volume and angle quantities: units and formatting.
//!
//! Like lengths (see [`crate::units`]), real-world quantities are carried in SI: areas in
//! square metres, volumes in cubic metres, angles in radians. Formatting is ASCII apart from
//! the degree sign, which is in WinAnsi and so can be drawn in a caption.
//!
//! | Quantity | Examples |
//! |---|---|
//! | Area | `12.50 sq ft`, `1.2 sq yd`, `0.25 acres`, `3.75 sq m`, `1.20 ha` |
//! | Volume | `27.00 cu ft`, `1.00 cu yd`, `2.500 cu m`, `750 L` |
//! | Angle | `45.0\u{b0}` |

use crate::units::{format_decimal, Unit, METRES_PER_INCH};

const METRES_PER_FOOT: f64 = METRES_PER_INCH * 12.0;
const METRES_PER_YARD: f64 = METRES_PER_FOOT * 3.0;

/// A unit of area.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AreaUnit {
    SqInch,
    SqFoot,
    SqYard,
    /// 43,560 square feet.
    Acre,
    SqMillimetre,
    SqCentimetre,
    SqMetre,
    /// 10,000 square metres.
    Hectare,
}

impl AreaUnit {
    /// Every unit, in a stable order suitable for a drop-down.
    pub const ALL: [AreaUnit; 8] = [
        AreaUnit::SqInch,
        AreaUnit::SqFoot,
        AreaUnit::SqYard,
        AreaUnit::Acre,
        AreaUnit::SqMillimetre,
        AreaUnit::SqCentimetre,
        AreaUnit::SqMetre,
        AreaUnit::Hectare,
    ];

    /// Square metres in one of this unit.
    pub fn sq_metres_per_unit(self) -> f64 {
        match self {
            AreaUnit::SqInch => METRES_PER_INCH * METRES_PER_INCH,
            AreaUnit::SqFoot => METRES_PER_FOOT * METRES_PER_FOOT,
            AreaUnit::SqYard => METRES_PER_YARD * METRES_PER_YARD,
            AreaUnit::Acre => 43_560.0 * METRES_PER_FOOT * METRES_PER_FOOT,
            AreaUnit::SqMillimetre => 1e-6,
            AreaUnit::SqCentimetre => 1e-4,
            AreaUnit::SqMetre => 1.0,
            AreaUnit::Hectare => 10_000.0,
        }
    }

    pub fn from_sq_metres(self, sq_metres: f64) -> f64 {
        sq_metres / self.sq_metres_per_unit()
    }

    pub fn to_sq_metres(self, value: f64) -> f64 {
        value * self.sq_metres_per_unit()
    }

    /// The label: `sq in`, `sq ft`, `sq yd`, `acres`, `sq mm`, `sq cm`, `sq m`, `ha`.
    pub fn abbreviation(self) -> &'static str {
        match self {
            AreaUnit::SqInch => "sq in",
            AreaUnit::SqFoot => "sq ft",
            AreaUnit::SqYard => "sq yd",
            AreaUnit::Acre => "acres",
            AreaUnit::SqMillimetre => "sq mm",
            AreaUnit::SqCentimetre => "sq cm",
            AreaUnit::SqMetre => "sq m",
            AreaUnit::Hectare => "ha",
        }
    }

    /// The unit with this label, as [`AreaUnit::abbreviation`] writes it.
    pub fn from_abbreviation(label: &str) -> Option<AreaUnit> {
        AreaUnit::ALL
            .into_iter()
            .find(|u| u.abbreviation() == label.trim())
    }

    /// The area unit that goes with a length unit: square feet for feet and inches, square
    /// metres for the metric units.
    pub fn for_length(unit: Unit) -> AreaUnit {
        match unit {
            Unit::Point | Unit::Inch => AreaUnit::SqInch,
            Unit::Foot | Unit::FeetInches => AreaUnit::SqFoot,
            Unit::Millimetre | Unit::Centimetre | Unit::Metre => AreaUnit::SqMetre,
        }
    }
}

/// A unit of volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VolumeUnit {
    CuInch,
    CuFoot,
    CuYard,
    CuCentimetre,
    CuMetre,
    Litre,
}

impl VolumeUnit {
    /// Every unit, in a stable order suitable for a drop-down.
    pub const ALL: [VolumeUnit; 6] = [
        VolumeUnit::CuInch,
        VolumeUnit::CuFoot,
        VolumeUnit::CuYard,
        VolumeUnit::CuCentimetre,
        VolumeUnit::CuMetre,
        VolumeUnit::Litre,
    ];

    /// Cubic metres in one of this unit.
    pub fn cu_metres_per_unit(self) -> f64 {
        match self {
            VolumeUnit::CuInch => METRES_PER_INCH.powi(3),
            VolumeUnit::CuFoot => METRES_PER_FOOT.powi(3),
            VolumeUnit::CuYard => METRES_PER_YARD.powi(3),
            VolumeUnit::CuCentimetre => 1e-6,
            VolumeUnit::CuMetre => 1.0,
            VolumeUnit::Litre => 1e-3,
        }
    }

    pub fn from_cu_metres(self, cu_metres: f64) -> f64 {
        cu_metres / self.cu_metres_per_unit()
    }

    pub fn to_cu_metres(self, value: f64) -> f64 {
        value * self.cu_metres_per_unit()
    }

    /// The label: `cu in`, `cu ft`, `cu yd`, `cu cm`, `cu m`, `L`.
    pub fn abbreviation(self) -> &'static str {
        match self {
            VolumeUnit::CuInch => "cu in",
            VolumeUnit::CuFoot => "cu ft",
            VolumeUnit::CuYard => "cu yd",
            VolumeUnit::CuCentimetre => "cu cm",
            VolumeUnit::CuMetre => "cu m",
            VolumeUnit::Litre => "L",
        }
    }

    /// The unit with this label, as [`VolumeUnit::abbreviation`] writes it.
    pub fn from_abbreviation(label: &str) -> Option<VolumeUnit> {
        VolumeUnit::ALL
            .into_iter()
            .find(|u| u.abbreviation() == label.trim())
    }

    /// The volume unit that goes with a length unit: cubic yards for feet (the unit concrete
    /// and earthworks are ordered in), cubic metres for the metric units.
    pub fn for_length(unit: Unit) -> VolumeUnit {
        match unit {
            Unit::Point | Unit::Inch => VolumeUnit::CuInch,
            Unit::Foot | Unit::FeetInches => VolumeUnit::CuYard,
            Unit::Millimetre | Unit::Centimetre | Unit::Metre => VolumeUnit::CuMetre,
        }
    }
}

/// An area unit plus a number of decimal places.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AreaFormat {
    pub unit: AreaUnit,
    /// Decimal places (capped at 12).
    pub places: u8,
}

impl AreaFormat {
    pub fn new(unit: AreaUnit, places: u8) -> AreaFormat {
        AreaFormat { unit, places }
    }

    /// Formats an area given in square metres. See [`format_area`].
    pub fn format(&self, sq_metres: f64) -> String {
        format_area(sq_metres, self)
    }
}

/// A volume unit plus a number of decimal places.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VolumeFormat {
    pub unit: VolumeUnit,
    /// Decimal places (capped at 12).
    pub places: u8,
}

impl VolumeFormat {
    pub fn new(unit: VolumeUnit, places: u8) -> VolumeFormat {
        VolumeFormat { unit, places }
    }

    /// Formats a volume given in cubic metres. See [`format_volume`].
    pub fn format(&self, cu_metres: f64) -> String {
        format_volume(cu_metres, self)
    }
}

fn number(value: f64, places: u8) -> String {
    if value.is_finite() {
        format_decimal(value, places)
    } else {
        format!("{value}")
    }
}

/// Formats an area given in **square metres**: `12.50 sq ft`.
pub fn format_area(sq_metres: f64, format: &AreaFormat) -> String {
    format!(
        "{} {}",
        number(format.unit.from_sq_metres(sq_metres), format.places),
        format.unit.abbreviation()
    )
}

/// Formats a volume given in **cubic metres**: `1.50 cu yd`.
pub fn format_volume(cu_metres: f64, format: &VolumeFormat) -> String {
    format!(
        "{} {}",
        number(format.unit.from_cu_metres(cu_metres), format.places),
        format.unit.abbreviation()
    )
}

/// Formats an angle given in **radians** as degrees with a degree sign: `45.0\u{b0}`.
pub fn format_angle(radians: f64, places: u8) -> String {
    format!("{}\u{b0}", number(radians.to_degrees(), places))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-12 * a.abs().max(b.abs()).max(1.0)
    }

    #[test]
    fn area_units_convert_exactly() {
        let ft2 = 0.09290304;
        assert!(close(AreaUnit::SqFoot.sq_metres_per_unit(), ft2));
        assert!(close(AreaUnit::SqInch.sq_metres_per_unit(), 0.00064516));
        assert!(close(AreaUnit::SqYard.sq_metres_per_unit(), 9.0 * ft2));
        assert!(close(AreaUnit::Acre.sq_metres_per_unit(), 4046.8564224));
        assert!(close(AreaUnit::Hectare.from_sq_metres(25_000.0), 2.5));
        assert!(close(AreaUnit::SqCentimetre.from_sq_metres(1.0), 10_000.0));
        assert!(close(AreaUnit::SqMillimetre.from_sq_metres(1.0), 1e6));
        for u in AreaUnit::ALL {
            assert!(close(u.from_sq_metres(u.to_sq_metres(7.25)), 7.25), "{u:?}");
            assert_eq!(AreaUnit::from_abbreviation(u.abbreviation()), Some(u));
        }
        assert_eq!(AreaUnit::from_abbreviation("sq furlongs"), None);
    }

    #[test]
    fn volume_units_convert_exactly() {
        let ft3 = 0.028316846592;
        assert!(close(VolumeUnit::CuFoot.cu_metres_per_unit(), ft3));
        assert!(close(VolumeUnit::CuYard.cu_metres_per_unit(), 27.0 * ft3));
        assert!(close(VolumeUnit::CuInch.cu_metres_per_unit(), 1.6387064e-5));
        assert!(close(VolumeUnit::Litre.from_cu_metres(0.75), 750.0));
        assert!(close(VolumeUnit::CuCentimetre.from_cu_metres(1.0), 1e6));
        for u in VolumeUnit::ALL {
            assert!(close(u.from_cu_metres(u.to_cu_metres(3.5)), 3.5), "{u:?}");
            assert_eq!(VolumeUnit::from_abbreviation(u.abbreviation()), Some(u));
        }
    }

    #[test]
    fn areas_are_formatted_with_unit_and_precision() {
        let ft2 = 0.09290304;
        let f = |unit, places| AreaFormat::new(unit, places);
        assert_eq!(
            format_area(12.5 * ft2, &f(AreaUnit::SqFoot, 2)),
            "12.50 sq ft"
        );
        assert_eq!(
            format_area(1200.0 * ft2, &f(AreaUnit::SqFoot, 0)),
            "1200 sq ft"
        );
        assert_eq!(
            format_area(18.0 * ft2, &f(AreaUnit::SqYard, 1)),
            "2.0 sq yd"
        );
        assert_eq!(
            format_area(21_780.0 * ft2, &f(AreaUnit::Acre, 3)),
            "0.500 acres"
        );
        assert_eq!(format_area(3.75, &f(AreaUnit::SqMetre, 2)), "3.75 sq m");
        assert_eq!(format_area(12_000.0, &f(AreaUnit::Hectare, 2)), "1.20 ha");
        assert_eq!(
            format_area(0.0005, &f(AreaUnit::SqCentimetre, 1)),
            "5.0 sq cm"
        );
        assert_eq!(format_area(-0.0, &f(AreaUnit::SqMetre, 2)), "0.00 sq m");
        assert_eq!(format_area(f64::NAN, &f(AreaUnit::SqMetre, 2)), "NaN sq m");
        assert_eq!(f(AreaUnit::SqMetre, 1).format(2.25), "2.2 sq m");
    }

    #[test]
    fn volumes_and_angles_are_formatted() {
        let ft3 = 0.028316846592;
        let f = |unit, places| VolumeFormat::new(unit, places);
        assert_eq!(
            format_volume(27.0 * ft3, &f(VolumeUnit::CuYard, 2)),
            "1.00 cu yd"
        );
        assert_eq!(
            format_volume(27.0 * ft3, &f(VolumeUnit::CuFoot, 1)),
            "27.0 cu ft"
        );
        assert_eq!(format_volume(2.5, &f(VolumeUnit::CuMetre, 3)), "2.500 cu m");
        assert_eq!(format_volume(0.75, &f(VolumeUnit::Litre, 0)), "750 L");
        assert_eq!(f(VolumeUnit::CuMetre, 2).format(1.0), "1.00 cu m");
        assert_eq!(format_angle(std::f64::consts::FRAC_PI_4, 1), "45.0\u{b0}");
        assert_eq!(format_angle(std::f64::consts::PI, 0), "180\u{b0}");
        assert_eq!(format_angle(1.0, 2), "57.30\u{b0}");
    }

    #[test]
    fn the_unit_that_goes_with_a_length_unit() {
        assert_eq!(AreaUnit::for_length(Unit::FeetInches), AreaUnit::SqFoot);
        assert_eq!(AreaUnit::for_length(Unit::Millimetre), AreaUnit::SqMetre);
        assert_eq!(VolumeUnit::for_length(Unit::Foot), VolumeUnit::CuYard);
        assert_eq!(VolumeUnit::for_length(Unit::Metre), VolumeUnit::CuMetre);
    }
}
