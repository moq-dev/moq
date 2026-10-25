//! The MoQ Epoch extension (draft-lcurley-moq-epoch-00).
//!
//! moq-transport names content by namespace alone, so a publisher that restarts at the
//! same namespace looks like the same broadcast to every relay. This extension carries
//! the publisher instance, the [`crate::Epoch`] moq-lite 07 puts on its routes and
//! requests, as an EPOCH parameter: on each advertisement (PUBLISH_NAMESPACE and
//! NAMESPACE), and on each request that resolves one (SUBSCRIBE, FETCH, and
//! TRACK_STATUS). A namespace and an EPOCH name one broadcast; a request carrying one
//! is refused rather than served by another instance.
//!
//! Negotiated on draft-17+ only, where SETUP is a Key-Value-Pair block, and only when
//! both endpoints declare the option.

use crate::Epoch;
use crate::coding::{DecodeError, Decoder, EncodeError, Encoder};

use super::{Param, Version};

/// EPOCH Setup Option: the sender understands the EPOCH parameter. Even, so the value
/// is a bare varint.
pub const EPOCH: u64 = 0x40B68;

/// EPOCH message parameter: the publisher instance, as a UUIDv7's 16 bytes. Odd, so the
/// value is length-prefixed.
pub const EPOCH_PARAM: u64 = 0x40B69;

/// Whether a version negotiates this extension: draft-17+, like MoQ Cluster.
pub fn supported(version: Version) -> bool {
	!matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16)
}

/// Whether the peer declared the option.
pub fn from_setup(params: &super::Parameters, version: Version) -> bool {
	supported(version) && params.get_varint(super::ParameterVarInt::Epoch).is_some()
}

/// Declare that we understand the EPOCH parameter.
pub fn into_setup(params: &mut super::Parameters, version: Version) {
	if supported(version) {
		params.set_varint(super::ParameterVarInt::Epoch, 1);
	}
}

impl Param for Epoch {
	fn param_encode(&self, w: &mut Encoder<'_>, _version: Version) -> Result<(), EncodeError> {
		w.bytes(&self.to_bytes())
	}

	/// Anything but a UUIDv7's 16 bytes is malformed.
	fn param_decode(r: &mut Decoder<'_>, _version: Version) -> Result<Self, DecodeError> {
		Epoch::from_bytes(r.bytes()?).map_err(|_| DecodeError::InvalidValue)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const VERSION: Version = Version::Draft18;

	#[test]
	fn code_point_parity() {
		// The parity is load-bearing: odd values are length-prefixed, even ones bare varints.
		assert_eq!(EPOCH % 2, 0);
		assert_eq!(EPOCH_PARAM % 2, 1);
	}

	#[test]
	fn only_draft17_and_later_negotiate() {
		for version in [Version::Draft14, Version::Draft16, Version::Draft17, Version::Draft22] {
			let mut params = super::super::Parameters::default();
			assert!(!from_setup(&params, version));
			into_setup(&mut params, version);
			assert_eq!(from_setup(&params, version), supported(version), "{version:?}");
		}
	}

	#[test]
	fn round_trips_and_refuses_anything_but_a_uuidv7() {
		let epoch = Epoch::mint();
		let mut buf = Vec::new();
		epoch
			.param_encode(&mut Encoder::new(&mut buf, VERSION.into()), VERSION)
			.unwrap();
		assert_eq!(buf.len(), 17, "a length byte, then the 16 bytes");
		let mut bytes = bytes::Bytes::from(buf);
		let decoded = crate::coding::decode_buf(&mut bytes, VERSION, Epoch::param_decode).unwrap();
		assert_eq!(decoded, epoch);

		for value in [
			Vec::new(),
			epoch.to_bytes()[..15].to_vec(),
			uuid::Builder::from_random_bytes([7; 16])
				.into_uuid()
				.as_bytes()
				.to_vec(),
		] {
			let mut buf = Vec::new();
			Encoder::new(&mut buf, VERSION.into()).bytes(&value).unwrap();
			let mut bytes = bytes::Bytes::from(buf);
			assert!(
				crate::coding::decode_buf(&mut bytes, VERSION, Epoch::param_decode).is_err(),
				"{value:?}"
			);
		}
	}
}
