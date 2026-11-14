use crate::{
	Instance, Path,
	coding::{DecodeError, Decoder, EncodeError, Encoder},
};

use super::Version;

/// Encode the instance a request pins as its UTF-8 bytes and minted flag, or empty and
/// unminted for none. Older versions carry nothing, so the peer resolves whichever instance
/// wins the path.
pub(super) fn encode_pin(
	w: &mut Encoder<'_>,
	version: Version,
	instance: Option<&Instance>,
) -> Result<(), EncodeError> {
	if !version.has_instance() {
		return Ok(());
	}
	match instance {
		Some(instance) => encode(w, instance),
		None => {
			w.bytes(&[])?;
			w.bool(false);
			Ok(())
		}
	}
}

/// Decode the instance a request pins: empty is none, and anything but UTF-8, or an empty
/// one flagged minted, is refused.
pub(super) fn decode_pin(r: &mut Decoder<'_>, version: Version) -> Result<Option<Instance>, DecodeError> {
	if !version.has_instance() {
		return Ok(None);
	}
	match (r.bytes()?, r.bool()?) {
		([], false) => Ok(None),
		(bytes, minted) => decode(bytes, minted).map(Some),
	}
}

/// Encode the publisher a request names, 0 letting the receiver select one, and how many
/// leading segments of the broadcast path make up the prefix of the route it follows. Only
/// lite-07 carries them, so an older peer always selects, and what is set here is dropped.
pub(super) fn encode_node(w: &mut Encoder<'_>, version: Version, node: u64, depth: u64) -> Result<(), EncodeError> {
	if !version.has_routes() {
		return Ok(());
	}
	// A route needs its publisher.
	if node == 0 && depth != 0 {
		return Err(EncodeError::InvalidState);
	}
	w.varint(node)?;
	w.varint(depth)
}

/// Decode the publisher a request for `broadcast` names, and its route's depth; see
/// [`encode_node`]. A depth past the path's segments names no prefix of it.
pub(super) fn decode_node(r: &mut Decoder<'_>, version: Version, broadcast: &Path) -> Result<(u64, u64), DecodeError> {
	if !version.has_routes() {
		return Ok((0, 0));
	}
	let node = r.varint()?;
	let depth = r.varint()?;
	if (node == 0 && depth != 0) || depth > broadcast.parts().count() as u64 {
		return Err(DecodeError::InvalidValue);
	}
	Ok((node, depth))
}

/// Encode the instance, its minted flag, and the epoch an announcement serves. Every route
/// carries an instance, so lite-07 refuses to announce one without. Older versions carry
/// neither, so the receiving session mints an instance of its own.
pub(super) fn encode_route(
	w: &mut Encoder<'_>,
	version: Version,
	instance: Option<&Instance>,
	epoch: u64,
) -> Result<(), EncodeError> {
	if !version.has_instance() {
		return Ok(());
	}
	encode(w, instance.ok_or(EncodeError::InvalidState)?)?;
	w.varint(epoch)
}

/// Decode the instance and epoch an announcement serves; see [`encode_route`].
pub(super) fn decode_route(r: &mut Decoder<'_>, version: Version) -> Result<(Option<Instance>, u64), DecodeError> {
	if !version.has_instance() {
		return Ok((None, 0));
	}
	let instance = decode(r.bytes()?, r.bool()?)?;
	Ok((Some(instance), r.varint()?))
}

/// Encode the instance of a copy a warm route names, which is never none.
pub(super) fn encode_copy(w: &mut Encoder<'_>, instance: &Instance) -> Result<(), EncodeError> {
	encode(w, instance)
}

/// Decode the instance of a copy a warm route names; see [`encode_copy`].
pub(super) fn decode_copy(r: &mut Decoder<'_>) -> Result<Instance, DecodeError> {
	decode(r.bytes()?, r.bool()?)
}

fn encode(w: &mut Encoder<'_>, instance: &Instance) -> Result<(), EncodeError> {
	w.bytes(instance.as_str().as_bytes())?;
	w.bool(instance.is_minted());
	Ok(())
}

fn decode(bytes: &[u8], minted: bool) -> Result<Instance, DecodeError> {
	let text = std::str::from_utf8(bytes).map_err(|_| DecodeError::InvalidValue)?;
	Instance::decoded(text, minted).map_err(|_| DecodeError::InvalidValue)
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

	/// The flag, not the text, says a session minted an instance: each round-trips as
	/// itself, and a named instance that looks like the old `~` mark stays named.
	#[test]
	fn lite07_carries_the_minted_flag() {
		let minted = Instance::minted();
		let decoded = pin(Version::Lite07, Some(&minted)).unwrap();
		assert!(decoded.is_minted());
		assert_eq!(decoded, minted);
		let named: Instance = "~named".parse().unwrap();
		assert!(!pin(Version::Lite07, Some(&named)).unwrap().is_minted());
	}

	#[test]
	fn refuses_a_bad_minted_flag() {
		let decode = |text: &[u8], flag: u8| {
			let mut buf = Vec::new();
			let w = &mut Encoder::new(&mut buf, Version::Lite07.into());
			w.bytes(text).unwrap();
			w.u8(flag);
			decode_pin(&mut Decoder::new(&buf, Version::Lite07.into()), Version::Lite07)
		};
		assert!(decode(b"a", 2).is_err());
		assert!(decode(b"", 1).is_err(), "no instance is never minted");
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
		w.bool(false);
		w.varint(0).unwrap();
		assert!(decode_route(&mut Decoder::new(&buf, Version::Lite07.into()), Version::Lite07).is_err());
	}

	#[test]
	fn refuses_non_utf8() {
		let mut buf = Vec::new();
		let w = &mut Encoder::new(&mut buf, Version::Lite07.into());
		w.bytes(&[0xff]).unwrap();
		w.bool(false);
		assert!(decode_pin(&mut Decoder::new(&buf, Version::Lite07.into()), Version::Lite07).is_err());
	}
}
