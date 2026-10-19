use bytes::Bytes;

use super::Version;
use crate::{
	Error, Timescale, Timestamp,
	coding::{Decoder, Encoder},
	model::object::{Encoding, Properties, Property, Value},
};

pub(super) fn encoding(version: Version) -> Encoding {
	match version {
		Version::Draft14 | Version::Draft15 => Encoding::Absolute,
		Version::Draft16 => Encoding::DeltaQuic,
		Version::Draft17 => Encoding::Delta { seven: false },
		_ => Encoding::Delta { seven: true },
	}
}

/// Draft-19 assigns property 0x06 to SUBGROUP_DELIVERY_TIMEOUT.
pub(super) fn has_delivery_timeout(version: Version) -> bool {
	!matches!(
		version,
		Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17 | Version::Draft18
	)
}

/// Only draft-14 lets a status object, such as END_OF_GROUP, carry properties.
#[cfg(feature = "async")]
pub(super) fn status_has_properties(version: Version) -> bool {
	version == Version::Draft14
}

/// Refuse properties on a status object where its draft forbids them.
#[cfg(feature = "async")]
pub(super) fn check_status(version: Version, properties: &Properties) -> Result<(), Error> {
	match status_has_properties(version) || properties.is_empty() {
		true => Ok(()),
		false => Err(Error::Session(crate::SessionError::ProtocolViolation)),
	}
}

pub(super) struct Decoded {
	pub properties: Properties,
	pub timestamp: Option<Timestamp>,
	pub gap: Option<u64>,
}

struct List {
	properties: Properties,
	time: Option<u64>,
	gap: Option<u64>,
	sealed_time: bool,
}

fn list(data: &Bytes, version: Version, immutable: bool) -> Result<List, Error> {
	let keys = encoding(version);
	let mut decoder = Decoder::new(data, version.into());
	let mut result = List {
		properties: Properties::default(),
		time: None,
		gap: None,
		sealed_time: false,
	};
	let mut previous = 0u64;
	while !decoder.is_empty() {
		let key = decoder.varint()?;
		let id = match keys {
			Encoding::Absolute => key,
			Encoding::Delta { .. } | Encoding::DeltaQuic => previous.checked_add(key).ok_or(Error::MalformedTrack)?,
		};
		previous = id;
		let value = if id % 2 == 0 {
			let value = decoder.varint()?;
			if id == 0x3e && result.gap.replace(value).is_some() {
				return Err(Error::MalformedTrack);
			}
			if id == 0x10 {
				result.time = Some(value);
				if result.properties.timestamp.replace(value).is_some() {
					return Err(Error::MalformedTrack);
				}
				continue;
			}
			Value::Int(value)
		} else {
			let bytes = decoder.bytes()?;
			if bytes.len() > u16::MAX as usize {
				return Err(Error::MalformedTrack);
			}
			// A small cached property must not pin the reader's larger receive buffer.
			let bytes = Bytes::copy_from_slice(bytes);
			if id == 0x0b {
				if immutable || result.properties.immutable.replace(keys).is_some() {
					return Err(Error::MalformedTrack);
				}
				let nested = list(&bytes, version, true).map_err(|_| Error::MalformedTrack)?;
				result.sealed_time = nested.time.is_some();
				result.time = result.time.or(nested.time);
				if let Some(gap) = nested.gap
					&& result.gap.replace(gap).is_some()
				{
					return Err(Error::MalformedTrack);
				}
			}
			Value::Bytes(bytes)
		};
		result.properties.values.push(Property { id, value });
	}
	Ok(result)
}

pub(super) fn read(data: &Bytes, version: Version, timescale: Option<Timescale>) -> Result<Decoded, Error> {
	let mut parsed = list(data, version, false)?;
	parsed.properties.timescale = timescale.filter(|_| parsed.time.is_some());
	parsed.properties.sealed_time = parsed.sealed_time;
	parsed.properties.delivery_timeout = has_delivery_timeout(version);
	let timestamp = match (parsed.time, timescale) {
		(Some(value), Some(timescale)) => Some(Timestamp::new(value, timescale).map_err(|_| Error::MalformedTrack)?),
		_ => None,
	};
	Ok(Decoded {
		properties: parsed.properties,
		timestamp,
		gap: parsed.gap,
	})
}

/// Serialize relayed properties for `version`, with Timestamps in the `timescale` the
/// egress declares, if any.
pub(super) fn write(properties: &Properties, version: Version, timescale: Option<Timescale>) -> Result<Vec<u8>, Error> {
	if properties
		.immutable
		.is_some_and(|original| original != encoding(version))
	{
		return Err(Error::Unsupported);
	}
	// An untimed ingress forwards its Timestamps as is. A timed one to an egress that
	// declares no units drops them, as a plain frame there carries none.
	let unstamped = properties.timescale.is_some() && timescale.is_none();
	let convert = match (properties.timescale, timescale) {
		(Some(from), Some(to)) if from != to => Some((from, to)),
		_ => None,
	};
	let time = |value: u64| -> Result<Value, Error> {
		Ok(Value::Int(match convert {
			Some((from, to)) => Timestamp::new(value, from)
				.and_then(|timestamp| timestamp.convert(to))
				.map_err(|_| Error::Unsupported)?
				.value(),
			None => value,
		}))
	};
	// Rewriting or dropping an immutable Timestamp would break it; forwarding it would
	// change its meaning.
	if properties.sealed_time && (convert.is_some() || unstamped) {
		return Err(Error::Unsupported);
	}
	// Across draft-19, forwarding 0x06 would give it a different meaning.
	// A mutable one is dropped; a sealed one is refused.
	let flipped = properties.delivery_timeout != has_delivery_timeout(version);
	if flipped && sealed_key(properties, version, 0x06)? {
		return Err(Error::Unsupported);
	}
	let timestamp = properties
		.timestamp
		.filter(|_| !unstamped)
		.map(|value| {
			Ok::<_, Error>(Property {
				id: 0x10,
				value: time(value)?,
			})
		})
		.transpose()?;
	let mut sorted: Vec<_> = properties
		.values
		.iter()
		.filter(|property| property.id != 0x06 || !flipped)
		.chain(timestamp.as_ref())
		.collect();
	// Delta keys require ascending order. Equal keys retain the publisher's order.
	sorted.sort_by_key(|property| property.id);
	let mut out = Vec::new();
	let mut encoder = Encoder::new(&mut out, version.into());
	let mut previous = 0;
	for property in sorted {
		encoder.varint(match encoding(version) {
			Encoding::Absolute => property.id,
			Encoding::Delta { .. } | Encoding::DeltaQuic => property.id - previous,
		})?;
		previous = property.id;
		match &property.value {
			Value::Int(value) => encoder.varint(*value)?,
			Value::Bytes(value) => encoder.bytes(value)?,
		}
	}
	Ok(out)
}

/// Whether the immutable block holds a property with `id`.
fn sealed_key(properties: &Properties, version: Version, id: u64) -> Result<bool, Error> {
	let Some(sealed) = properties.values.iter().find(|property| property.id == 0x0b) else {
		return Ok(false);
	};
	let Value::Bytes(bytes) = &sealed.value else {
		return Err(Error::MalformedTrack);
	};
	let nested = list(bytes, version, true)?;
	Ok(nested.properties.values.iter().any(|property| property.id == id))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn key_six_is_opaque_on_every_draft() {
		for version in [
			Version::Draft14,
			Version::Draft15,
			Version::Draft16,
			Version::Draft17,
			Version::Draft18,
			Version::Draft19,
			Version::Draft20,
			Version::Draft21,
			Version::Draft22,
		] {
			for bytes in [
				Bytes::from_static(&[0x06, 42]),
				Bytes::from_static(&[0x0b, 2, 0x06, 42]),
			] {
				let decoded = read(&bytes, version, Some(Timescale::MILLI)).unwrap();
				assert_eq!(decoded.timestamp, None, "{version}");
				assert!(!decoded.properties.sealed_time, "{version}");
				for timescale in [Some(Timescale::MICRO), None] {
					assert_eq!(
						write(&decoded.properties, version, timescale).unwrap(),
						bytes,
						"{version}"
					);
				}
			}
		}
	}

	#[test]
	fn cached_bytes_do_not_retain_the_receive_buffer() {
		use std::sync::{
			Arc,
			atomic::{AtomicBool, Ordering},
		};
		struct Buffer {
			bytes: Vec<u8>,
			dropped: Arc<AtomicBool>,
		}
		impl AsRef<[u8]> for Buffer {
			fn as_ref(&self) -> &[u8] {
				&self.bytes
			}
		}
		impl Drop for Buffer {
			fn drop(&mut self) {
				self.dropped.store(true, Ordering::Relaxed);
			}
		}
		let dropped = Arc::new(AtomicBool::new(false));
		let mut bytes = vec![0; 65536];
		bytes[..3].copy_from_slice(&[5, 1, 42]);
		let received = Bytes::from_owner(Buffer {
			bytes,
			dropped: dropped.clone(),
		})
		.slice(..3);
		let decoded = read(&received, Version::Draft22, None).unwrap();
		drop(received);
		assert!(dropped.load(Ordering::Relaxed));
		assert_eq!(
			decoded.properties.values[0].value,
			Value::Bytes(Bytes::from_static(&[42]))
		);
	}

	#[test]
	fn immutable_timestamp_is_read_without_adding_an_outer_property() {
		let bytes = Bytes::from_static(&[0x0b, 2, 0x10, 42]);
		let decoded = read(&bytes, Version::Draft22, Some(Timescale::MILLI)).unwrap();
		assert_eq!(decoded.timestamp, Some(Timestamp::from_millis(42).unwrap()));
		assert_eq!(decoded.properties.timestamp, None);
		let milli = Some(Timescale::MILLI);
		assert_eq!(write(&decoded.properties, Version::Draft18, milli).unwrap(), bytes);
		assert!(matches!(
			write(&decoded.properties, Version::Draft17, milli),
			Err(Error::Unsupported)
		));
		// Its units cannot follow the egress's, or vanish with them, without rewriting the block.
		for timescale in [Some(Timescale::MICRO), None] {
			assert!(matches!(
				write(&decoded.properties, Version::Draft18, timescale),
				Err(Error::Unsupported)
			));
		}
		// An untimed ingress has no units to lose.
		let untimed = read(&bytes, Version::Draft22, None).unwrap();
		assert_eq!(write(&untimed.properties, Version::Draft18, None).unwrap(), bytes);
	}

	/// The cache keeps a timed track in its own units, so a relayed Timestamp must leave
	/// in the units the egress declares, not the ones it arrived in.
	#[test]
	fn mutable_timestamps_follow_the_egress_units() {
		for version in [Version::Draft18, Version::Draft22] {
			let bytes = Bytes::from_static(&[0x10, 42, 0x20, 7]);
			let decoded = read(&bytes, version, Some(Timescale::MILLI)).unwrap();
			let written = write(&decoded.properties, version, Some(Timescale::MICRO)).unwrap();
			let again = read(&Bytes::from(written), version, Some(Timescale::MICRO)).unwrap();
			let micros = Timestamp::new(42_000, Timescale::MICRO).unwrap();
			assert_eq!(again.timestamp, Some(micros), "{version}");
			assert_eq!(again.properties.values.last().unwrap().value, Value::Int(7));

			// An egress that declares no units drops it, as a plain frame there has none.
			let written = write(&decoded.properties, version, None).unwrap();
			let again = read(&Bytes::from(written), version, None).unwrap();
			assert_eq!(again.properties.timestamp, None, "{version}");
			assert_eq!(again.properties.values.len(), 1, "{version}");
			assert_eq!(again.properties.values[0].value, Value::Int(7));
			// An untimed ingress forwards it untouched.
			let untimed = read(&bytes, version, None).unwrap();
			assert_eq!(write(&untimed.properties, version, None).unwrap(), bytes);
		}
	}

	/// Draft-19 assigns 0x06 to SUBGROUP_DELIVERY_TIMEOUT, so it cannot cross that boundary.
	#[test]
	fn key_six_follows_its_draft() {
		let bytes = Bytes::from_static(&[0x06, 42, 0x1a, 7]);
		let milli = Some(Timescale::MILLI);
		let decoded = read(&bytes, Version::Draft22, milli).unwrap();
		assert_eq!(decoded.timestamp, None);
		for timescale in [Some(Timescale::MICRO), None] {
			assert_eq!(write(&decoded.properties, Version::Draft19, timescale).unwrap(), bytes);
		}

		// Across draft-19 neither reading survives: a mutable one is dropped.
		let modern = Bytes::from_static(&[0x20, 7]);
		assert_eq!(write(&decoded.properties, Version::Draft18, milli).unwrap(), modern);
		let legacy = read(&bytes, Version::Draft18, milli).unwrap();
		assert_eq!(write(&legacy.properties, Version::Draft19, milli).unwrap(), modern);

		// A sealed one is refused.
		let sealed = Bytes::from_static(&[0x0b, 2, 0x06, 42]);
		let decoded = read(&sealed, Version::Draft22, milli).unwrap();
		assert!(!decoded.properties.sealed_time);
		assert_eq!(write(&decoded.properties, Version::Draft22, None).unwrap(), sealed);
		assert!(matches!(
			write(&decoded.properties, Version::Draft18, milli),
			Err(Error::Unsupported)
		));
		let legacy = read(&sealed, Version::Draft18, milli).unwrap();
		assert!(matches!(
			write(&legacy.properties, Version::Draft19, milli),
			Err(Error::Unsupported)
		));
	}

	#[test]
	fn immutable_lists_validate_nested_blocks_and_integer_forms() {
		for bytes in [&[0x0b, 2, 0x0b, 0][..], &[0x0b, 0, 0, 0][..], &[0x0b, 1, 2][..]] {
			assert!(read(&Bytes::copy_from_slice(bytes), Version::Draft22, None).is_err());
		}
		// Draft-18 permits this nonminimal seven-byte value; draft-17 cannot read it.
		let bytes = Bytes::from_static(&[0x0b, 8, 2, 0xfc, 0, 0, 0, 0, 0, 1]);
		let decoded = read(&bytes, Version::Draft18, None).unwrap();
		assert_eq!(write(&decoded.properties, Version::Draft22, None).unwrap(), bytes);
		assert!(read(&bytes, Version::Draft17, None).is_err());
		assert!(matches!(
			write(&decoded.properties, Version::Draft17, None),
			Err(Error::Unsupported)
		));
	}
}
