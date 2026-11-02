use crate::{
	Instance,
	coding::{DecodeError, Decoder, EncodeError, Encoder},
};

use super::Version;

/// Encode the instance a request pins as its UTF-8 bytes, or empty for none. Older
/// versions carry nothing, so the peer resolves whichever instance wins the path.
pub(super) fn encode_pin(
	w: &mut Encoder<'_>,
	version: Version,
	instance: Option<&Instance>,
) -> Result<(), EncodeError> {
	if !version.has_instance() {
		return Ok(());
	}
	w.bytes(instance.map_or(&[][..], |instance| instance.as_str().as_bytes()))
}

/// Decode the instance a request pins: empty is none, and anything but UTF-8 is refused.
pub(super) fn decode_pin(r: &mut Decoder<'_>, version: Version) -> Result<Option<Instance>, DecodeError> {
	if !version.has_instance() {
		return Ok(None);
	}
	match r.bytes()? {
		[] => Ok(None),
		bytes => decode(bytes).map(Some),
	}
}

/// Encode the instance and epoch an announcement serves. Every route carries an instance,
/// so lite-07 refuses to announce one without. Older versions carry neither, so the
/// receiving session mints an instance of its own.
pub(super) fn encode_route(
	w: &mut Encoder<'_>,
	version: Version,
	instance: Option<&Instance>,
	epoch: u64,
) -> Result<(), EncodeError> {
	if !version.has_instance() {
		return Ok(());
	}
	let instance = instance.ok_or(EncodeError::InvalidState)?;
	w.bytes(instance.as_str().as_bytes())?;
	w.varint(epoch)
}

/// Decode the instance and epoch an announcement serves; see [`encode_route`].
pub(super) fn decode_route(r: &mut Decoder<'_>, version: Version) -> Result<(Option<Instance>, u64), DecodeError> {
	if !version.has_instance() {
		return Ok((None, 0));
	}
	let instance = decode(r.bytes()?)?;
	Ok((Some(instance), r.varint()?))
}

fn decode(bytes: &[u8]) -> Result<Instance, DecodeError> {
	std::str::from_utf8(bytes)
		.map_err(|_| DecodeError::InvalidValue)?
		.parse()
		.map_err(|_| DecodeError::InvalidValue)
}

#[cfg(test)]
mod test {
	use super::*;

	fn pin(version: Version, instance: Option<&Instance>) -> Option<Instance> {
		let mut buf = Vec::new();
		encode_pin(&mut Encoder::new(&mut buf, version.into()), version, instance).unwrap();
		let mut decoder = Decoder::new(&buf, version.into());
		let decoded = decode_pin(&mut decoder, version).unwrap();
		assert!(decoder.is_empty());
		decoded
	}

	#[test]
	fn lite07_carries_the_pin() {
		let instance = Instance::random();
		assert_eq!(pin(Version::Lite07, Some(&instance)), Some(instance));
		assert_eq!(pin(Version::Lite07, None), None);
	}

	#[test]
	fn older_versions_drop_it() {
		assert_eq!(pin(Version::Lite06, Some(&Instance::random())), None);
	}

	#[test]
	fn a_route_carries_its_instance_and_epoch() {
		let instance = Instance::random();
		let mut buf = Vec::new();
		encode_route(
			&mut Encoder::new(&mut buf, Version::Lite07.into()),
			Version::Lite07,
			Some(&instance),
			7,
		)
		.unwrap();
		let mut decoder = Decoder::new(&buf, Version::Lite07.into());
		assert_eq!(
			decode_route(&mut decoder, Version::Lite07).unwrap(),
			(Some(instance), 7)
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
