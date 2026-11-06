//! Negotiation for DEFLATE-compressed tracks (draft-lcurley-moq-flate).

use crate::coding::DecodeError;

use super::{ParameterVarInt, Parameters, Version};

/// FLATE Setup Option, whose value 1 offers the group-scoped DEFLATE format.
pub(super) const OPTION: u64 = 0x40B6A;
/// DEFLATE track codec, in the MOQ Properties registry.
pub(super) const COMPRESSION: u64 = 0x40B6C;
/// Inflated Size object property, in the MOQ Properties registry.
pub(super) const INFLATED: u64 = 0x40B6E;

pub(crate) fn into_setup(parameters: &mut Parameters) {
	parameters.set_varint(ParameterVarInt::Flate, 1);
}

pub(crate) fn from_setup(parameters: &Parameters) -> Result<bool, DecodeError> {
	match parameters.get_varint(ParameterVarInt::Flate) {
		None => Ok(false),
		Some(1) => Ok(true),
		Some(_) => Err(DecodeError::InvalidValue),
	}
}

/// A sealed declaration cannot accompany the plain fallback for an incompatible peer.
pub(crate) fn egress(info: &crate::track::Info, compression: bool) -> Result<(), crate::Error> {
	if !compression
		&& info
			.properties
			.as_ref()
			.is_some_and(|p| p.sealed().any(|id| id == COMPRESSION))
	{
		return Err(crate::Error::Unsupported);
	}
	Ok(())
}

/// A sealed object size cannot describe a plain fallback on another protocol either.
pub(crate) fn egress_object(
	properties: &crate::model::object::Properties,
	source: Option<Version>,
	compression: bool,
) -> Result<(), crate::Error> {
	let Some(block) = properties.block.as_ref().filter(|b| !compression && b.immutable) else {
		return Ok(());
	};
	super::object::compression(block.bytes.to_vec(), source.ok_or(crate::Error::MalformedTrack)?, None)?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn negotiation_requires_an_offer() {
		for version in crate::Versions::all().iter().filter_map(|v| Version::try_from(*v).ok()) {
			let mut parameters = Parameters::default();
			assert!(!from_setup(&parameters).unwrap());
			into_setup(&mut parameters);
			assert!(from_setup(&parameters).unwrap());
			use crate::coding::{Decode, Encode};
			let encoded = parameters.encode_bytes(version).unwrap();
			let (decoded, consumed) = Parameters::decode_slice(&encoded, version).unwrap();
			assert_eq!(consumed, encoded.len());
			assert!(from_setup(&decoded).unwrap());
			for invalid in [0, 2, u64::MAX >> 2] {
				parameters.set_varint(ParameterVarInt::Flate, invalid);
				assert!(from_setup(&parameters).is_err());
			}
		}
	}

	#[test]
	fn setup_code_point_is_even_and_not_grease() {
		assert_eq!(OPTION % 2, 0);
		assert_ne!((OPTION - 0x9d) % 0x7f, 0);
		const { assert!(OPTION >= 0x4000) };
	}
}
