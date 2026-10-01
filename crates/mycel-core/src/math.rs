//! Integer math conventions for authoritative 2D simulation state.
//!
//! Positions use milli-world-units, +X points right, +Y points down, and angle
//! units increase clockwise. Rendering may convert these values to floats at the
//! presentation boundary; simulation state does not use floating-point math.

use crate::{CanonicalState, CanonicalWriter};

const MILLI_UNITS_PER_WORLD_UNIT: i128 = 1_000;

/// Fixed-point simulation scalar with one-thousandth-world-unit precision.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SimScalar(i64);

impl CanonicalState for SimScalar {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_i64(self.0);
    }
}

impl SimScalar {
    /// Zero world units.
    pub const ZERO: Self = Self(0);
    /// One world unit.
    pub const ONE: Self = Self(1_000);

    /// Constructs a scalar from its milli-world-unit representation.
    #[must_use]
    pub const fn from_milli_units(value: i64) -> Self {
        Self(value)
    }

    /// Returns the milli-world-unit representation.
    #[must_use]
    pub const fn milli_units(self) -> i64 {
        self.0
    }

    /// Adds two values without wrapping or saturation.
    ///
    /// # Errors
    ///
    /// Returns [`MathError::Overflow`] when the result does not fit in `i64`.
    pub fn checked_add(self, other: Self) -> Result<Self, MathError> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(MathError::Overflow)
    }

    /// Subtracts two values without wrapping or saturation.
    ///
    /// # Errors
    ///
    /// Returns [`MathError::Overflow`] when the result does not fit in `i64`.
    pub fn checked_sub(self, other: Self) -> Result<Self, MathError> {
        self.0
            .checked_sub(other.0)
            .map(Self)
            .ok_or(MathError::Overflow)
    }

    /// Multiplies values and truncates the result toward zero to milli-unit
    /// precision.
    ///
    /// # Errors
    ///
    /// Returns [`MathError::Overflow`] when the result does not fit in `i64`.
    pub fn checked_mul(self, other: Self) -> Result<Self, MathError> {
        let product = i128::from(self.0) * i128::from(other.0);
        let milli_units = product / MILLI_UNITS_PER_WORLD_UNIT;
        i64::try_from(milli_units)
            .map(Self)
            .map_err(|_| MathError::Overflow)
    }

    /// Divides values and truncates the result toward zero to milli-unit
    /// precision.
    ///
    /// # Errors
    ///
    /// Returns [`MathError::DivisionByZero`] for a zero divisor, or
    /// [`MathError::Overflow`] when the result does not fit in `i64`.
    pub fn checked_div(self, other: Self) -> Result<Self, MathError> {
        if other.0 == 0 {
            return Err(MathError::DivisionByZero);
        }
        let scaled_numerator = i128::from(self.0) * MILLI_UNITS_PER_WORLD_UNIT;
        let milli_units = scaled_numerator / i128::from(other.0);
        i64::try_from(milli_units)
            .map(Self)
            .map_err(|_| MathError::Overflow)
    }
}

/// Two-dimensional position, direction, or scale.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Vec2 {
    pub x: SimScalar,
    pub y: SimScalar,
}

impl CanonicalState for Vec2 {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        self.x.write_canonical(writer);
        self.y.write_canonical(writer);
    }
}

impl Vec2 {
    /// Zero vector.
    pub const ZERO: Self = Self::new(SimScalar::ZERO, SimScalar::ZERO);

    /// Constructs a vector from its coordinates.
    #[must_use]
    pub const fn new(x: SimScalar, y: SimScalar) -> Self {
        Self { x, y }
    }

    /// Adds vectors componentwise without wrapping or saturation.
    ///
    /// # Errors
    ///
    /// Returns [`MathError::Overflow`] if either coordinate does not fit in
    /// `i64`.
    pub fn checked_add(self, other: Self) -> Result<Self, MathError> {
        Ok(Self::new(
            self.x.checked_add(other.x)?,
            self.y.checked_add(other.y)?,
        ))
    }

    /// Subtracts vectors componentwise without wrapping or saturation.
    ///
    /// # Errors
    ///
    /// Returns [`MathError::Overflow`] if either coordinate does not fit in
    /// `i64`.
    pub fn checked_sub(self, other: Self) -> Result<Self, MathError> {
        Ok(Self::new(
            self.x.checked_sub(other.x)?,
            self.y.checked_sub(other.y)?,
        ))
    }
}

/// Clockwise angle measured in 1/65,536ths of a full turn.
///
/// The integer representation wraps naturally at one full turn. Convert to
/// backend-specific radians only when producing presentation data.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Angle(u16);

impl CanonicalState for Angle {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_u16(self.0);
    }
}

impl Angle {
    /// Zero degrees.
    pub const ZERO: Self = Self(0);
    /// A clockwise quarter turn.
    pub const QUARTER_TURN: Self = Self(16_384);
    /// A half turn.
    pub const HALF_TURN: Self = Self(32_768);
    /// A clockwise three-quarter turn.
    pub const THREE_QUARTER_TURN: Self = Self(49_152);

    /// Constructs an angle from canonical unsigned turn units in `0..65_536`.
    #[must_use]
    pub const fn from_turn_units(units: u16) -> Self {
        Self(units)
    }

    /// Returns the angle's canonical turn-unit representation.
    #[must_use]
    pub const fn turn_units(self) -> u16 {
        self.0
    }
}

/// Position, clockwise rotation, and nonuniform scale in 2D simulation space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Transform2D {
    pub translation: Vec2,
    pub rotation: Angle,
    pub scale: Vec2,
}

impl CanonicalState for Transform2D {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        self.translation.write_canonical(writer);
        self.rotation.write_canonical(writer);
        self.scale.write_canonical(writer);
    }
}

impl Default for Transform2D {
    fn default() -> Self {
        Self {
            translation: Vec2::ZERO,
            rotation: Angle::ZERO,
            scale: Vec2::new(SimScalar::ONE, SimScalar::ONE),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathError {
    Overflow,
    DivisionByZero,
}

impl std::fmt::Display for MathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Overflow => f.write_str("fixed-point simulation math overflowed"),
            Self::DivisionByZero => f.write_str("cannot divide a simulation scalar by zero"),
        }
    }
}

impl std::error::Error for MathError {}

#[cfg(test)]
mod tests {
    use super::{Angle, MathError, SimScalar, Transform2D, Vec2};

    #[test]
    fn fixed_point_arithmetic_is_exact_to_milli_unit_precision() {
        let two = SimScalar::from_milli_units(2_000);
        let half = SimScalar::from_milli_units(500);
        assert_eq!(two.checked_mul(half), Ok(SimScalar::ONE));
        assert_eq!(
            two.checked_div(half),
            Ok(SimScalar::from_milli_units(4_000))
        );
    }

    #[test]
    fn fixed_point_division_truncates_toward_zero() {
        let one = SimScalar::ONE;
        let three = SimScalar::from_milli_units(3_000);
        assert_eq!(one.checked_div(three), Ok(SimScalar::from_milli_units(333)));
        assert_eq!(
            SimScalar::from_milli_units(-1_000).checked_div(three),
            Ok(SimScalar::from_milli_units(-333))
        );
    }

    #[test]
    fn arithmetic_reports_overflow_and_division_by_zero() {
        assert_eq!(
            SimScalar::from_milli_units(i64::MAX).checked_add(SimScalar::ONE),
            Err(MathError::Overflow)
        );
        assert_eq!(
            SimScalar::ONE.checked_div(SimScalar::ZERO),
            Err(MathError::DivisionByZero)
        );
    }

    #[test]
    fn default_transform_is_identity_and_angle_uses_canonical_turn_units() {
        let transform = Transform2D::default();
        assert_eq!(transform.translation, Vec2::ZERO);
        assert_eq!(transform.rotation, Angle::ZERO);
        assert_eq!(transform.scale, Vec2::new(SimScalar::ONE, SimScalar::ONE));
        assert_eq!(Angle::from_turn_units(u16::MAX).turn_units(), u16::MAX);
    }
}
