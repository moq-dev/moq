//! DTLS 1.2 curve type definitions.
//!
//! NamedGroup is now in crate::types as it's shared between DTLS versions.
//! CurveType is DTLS 1.2 specific (used in ServerKeyExchange).

use nom::IResult;
use nom::number::complete::be_u8;
use std::fmt;

/// Curve type for ECDH parameters in DTLS 1.2.
///
/// This is specific to DTLS 1.2's ServerKeyExchange message format.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CurveType(u8);

#[allow(non_upper_case_globals)]
impl CurveType {
    /// Explicit prime curve parameters.
    pub const ExplicitPrime: Self = Self(1);
    /// Explicit characteristic-2 curve parameters.
    pub const ExplicitChar2: Self = Self(2);
    /// Named curve (the common case).
    pub const NamedCurve: Self = Self(3);

    /// Convert a u8 value to a `CurveType`.
    pub const fn from_u8(value: u8) -> Self {
        Self(value)
    }

    /// Convert this `CurveType` to its u8 value.
    pub const fn as_u8(&self) -> u8 {
        self.0
    }

    const fn is_unknown(&self) -> bool {
        !matches!(*self, Self(1..=3))
    }

    /// Parse a `CurveType` from wire format.
    pub fn parse(input: &[u8]) -> IResult<&[u8], CurveType> {
        let (input, value) = be_u8(input)?;
        Ok((input, CurveType::from_u8(value)))
    }
}

impl fmt::Debug for CurveType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_unknown() {
            return f.debug_tuple("Unknown").field(&self.0).finish();
        }

        let name = match *self {
            CurveType::ExplicitPrime => "ExplicitPrime",
            CurveType::ExplicitChar2 => "ExplicitChar2",
            CurveType::NamedCurve => "NamedCurve",
            _ => unreachable!("known DTLS 1.2 curve type missing Debug label"),
        };

        f.write_str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_type_newtype_shape() {
        assert_eq!(std::mem::size_of::<CurveType>(), 1);
    }

    #[test]
    fn curve_type_wire_roundtrip() {
        for curve_type in [
            CurveType::ExplicitPrime,
            CurveType::ExplicitChar2,
            CurveType::NamedCurve,
        ] {
            assert_eq!(CurveType::from_u8(curve_type.as_u8()), curve_type);
            assert!(!curve_type.is_unknown());
        }

        let unknown = CurveType::from_u8(0xFF);
        assert_eq!(unknown.as_u8(), 0xFF);
        assert!(unknown.is_unknown());
    }

    #[test]
    fn curve_type_debug_stays_enum_like() {
        assert_eq!(format!("{:?}", CurveType::NamedCurve), "NamedCurve");
        assert_eq!(format!("{:?}", CurveType::from_u8(0xFF)), "Unknown(255)");
    }
}
