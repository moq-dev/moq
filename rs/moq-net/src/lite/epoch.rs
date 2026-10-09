use crate::{
	Epoch,
	coding::{DecodeError, Decoder, EncodeError, Encoder},
};

use super::Version;

/// Encode an optional epoch as its 16 bytes, or empty for none. Older versions
/// carry nothing, so the peer sees a route or request of unknown identity.
pub(super) fn encode_epoch(w: &mut Encoder<'_>, version: Version, epoch: Option<&Epoch>) -> Result<(), EncodeError> {
	if !version.has_epoch() {
		return Ok(());
	}
	match epoch {
		Some(epoch) => w.bytes(&epoch.to_bytes()),
		None => w.bytes(&[]),
	}
}

/// Decode an optional epoch: empty is none, anything but a UUIDv7 is refused.
pub(super) fn decode_epoch(r: &mut Decoder<'_>, version: Version) -> Result<Option<Epoch>, DecodeError> {
	if !version.has_epoch() {
		return Ok(None);
	}
	match r.bytes()? {
		[] => Ok(None),
		bytes => Epoch::from_bytes(bytes)
			.map(Some)
			.map_err(|_| DecodeError::InvalidValue),
	}
}

#[cfg(test)]
mod test {
	use super::*;

	fn roundtrip(version: Version, epoch: Option<&Epoch>) -> Option<Epoch> {
		let mut buf = Vec::new();
		encode_epoch(&mut Encoder::new(&mut buf, version.into()), version, epoch).unwrap();
		let mut decoder = Decoder::new(&buf, version.into());
		let decoded = decode_epoch(&mut decoder, version).unwrap();
		assert!(decoder.is_empty());
		decoded
	}

	#[test]
	fn lite07_carries_the_epoch() {
		let epoch = Epoch::mint();
		assert_eq!(roundtrip(Version::Lite07, Some(&epoch)), Some(epoch));
		assert_eq!(roundtrip(Version::Lite07, None), None);
	}

	#[test]
	fn older_versions_drop_it() {
		let epoch = Epoch::mint();
		assert_eq!(roundtrip(Version::Lite06, Some(&epoch)), None);
	}

	#[test]
	fn refuses_a_non_v7_uuid() {
		let mut buf = Vec::new();
		Encoder::new(&mut buf, Version::Lite07.into())
			.bytes(uuid::Builder::from_random_bytes([7; 16]).into_uuid().as_bytes())
			.unwrap();
		let mut decoder = Decoder::new(&buf, Version::Lite07.into());
		assert!(decode_epoch(&mut decoder, Version::Lite07).is_err());
	}
}
