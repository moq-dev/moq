use crate::{
	Identity,
	coding::{DecodeError, Decoder, EncodeError, Encoder},
};

use super::Version;

/// Encode the identity a request pins as its UTF-8 bytes, or empty for none. Older
/// versions carry nothing, so the peer resolves whichever identity wins the path.
pub(super) fn encode_pin(
	w: &mut Encoder<'_>,
	version: Version,
	identity: Option<&Identity>,
) -> Result<(), EncodeError> {
	if !version.has_identity() {
		return Ok(());
	}
	w.bytes(identity.map_or(&[][..], |identity| identity.as_str().as_bytes()))
}

/// Decode the identity a request pins: empty is none, and anything but UTF-8 is refused.
pub(super) fn decode_pin(r: &mut Decoder<'_>, version: Version) -> Result<Option<Identity>, DecodeError> {
	if !version.has_identity() {
		return Ok(None);
	}
	match r.bytes()? {
		[] => Ok(None),
		bytes => decode(bytes).map(Some),
	}
}

/// Encode the identity and epoch an announcement serves. Every route carries an identity,
/// so lite-07 refuses to announce one without. Older versions carry neither, so the
/// receiving session mints an identity of its own.
pub(super) fn encode_route(
	w: &mut Encoder<'_>,
	version: Version,
	identity: Option<&Identity>,
	epoch: u64,
) -> Result<(), EncodeError> {
	if !version.has_identity() {
		return Ok(());
	}
	let identity = identity.ok_or(EncodeError::InvalidState)?;
	w.bytes(identity.as_str().as_bytes())?;
	w.varint(epoch)
}

/// Decode the identity and epoch an announcement serves; see [`encode_route`].
pub(super) fn decode_route(r: &mut Decoder<'_>, version: Version) -> Result<(Option<Identity>, u64), DecodeError> {
	if !version.has_identity() {
		return Ok((None, 0));
	}
	let identity = decode(r.bytes()?)?;
	Ok((Some(identity), r.varint()?))
}

fn decode(bytes: &[u8]) -> Result<Identity, DecodeError> {
	std::str::from_utf8(bytes)
		.map_err(|_| DecodeError::InvalidValue)?
		.parse()
		.map_err(|_| DecodeError::InvalidValue)
}

#[cfg(test)]
mod test {
	use super::*;

	fn pin(version: Version, identity: Option<&Identity>) -> Option<Identity> {
		let mut buf = Vec::new();
		encode_pin(&mut Encoder::new(&mut buf, version.into()), version, identity).unwrap();
		let mut decoder = Decoder::new(&buf, version.into());
		let decoded = decode_pin(&mut decoder, version).unwrap();
		assert!(decoder.is_empty());
		decoded
	}

	#[test]
	fn lite07_carries_the_pin() {
		let identity = Identity::random();
		assert_eq!(pin(Version::Lite07, Some(&identity)), Some(identity));
		assert_eq!(pin(Version::Lite07, None), None);
	}

	#[test]
	fn older_versions_drop_it() {
		assert_eq!(pin(Version::Lite06, Some(&Identity::random())), None);
	}

	#[test]
	fn a_route_carries_its_identity_and_epoch() {
		let identity = Identity::random();
		let mut buf = Vec::new();
		encode_route(
			&mut Encoder::new(&mut buf, Version::Lite07.into()),
			Version::Lite07,
			Some(&identity),
			7,
		)
		.unwrap();
		let mut decoder = Decoder::new(&buf, Version::Lite07.into());
		assert_eq!(
			decode_route(&mut decoder, Version::Lite07).unwrap(),
			(Some(identity), 7)
		);
		assert!(decoder.is_empty());

		// Every lite-07 route names one: none is refused both ways.
		assert!(
			encode_route(
				&mut Encoder::new(&mut Vec::new(), Version::Lite07.into()),
				Version::Lite07,
				None,
				0
			)
			.is_err()
		);
		let mut buf = Vec::new();
		let w = &mut Encoder::new(&mut buf, Version::Lite07.into());
		w.bytes(&[]).unwrap();
		w.varint(0).unwrap();
		assert!(decode_route(&mut Decoder::new(&buf, Version::Lite07.into()), Version::Lite07).is_err());
	}

	#[test]
	fn refuses_non_utf8() {
		let mut buf = Vec::new();
		Encoder::new(&mut buf, Version::Lite07.into()).bytes(&[0xff]).unwrap();
		assert!(decode_pin(&mut Decoder::new(&buf, Version::Lite07.into()), Version::Lite07).is_err());
	}
}
