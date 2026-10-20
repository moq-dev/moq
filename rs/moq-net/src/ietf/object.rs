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

pub(crate) struct Decoded {
	pub properties: Properties,
	pub timestamp: Option<Timestamp>,
	pub gap: Option<u64>,
}

/// The largest properties block an object may carry, which a relay forwards unchanged.
pub(crate) const MAX_BLOCK: usize = super::group::MAX_OBJECT_EXTENSIONS;

struct List {
	/// In arrival order, without the mutable Timestamp. Values borrow the input.
	values: Vec<Property>,
	timestamp: Option<u64>,
	time: Option<u64>,
	gap: Option<u64>,
	sealed_time: Option<u64>,
	immutable: bool,
}

fn list(data: &Bytes, version: Version, immutable: bool) -> Result<List, Error> {
	let keys = encoding(version);
	let mut decoder = Decoder::new(data, version.into());
	let mut result = List {
		values: Vec::new(),
		timestamp: None,
		time: None,
		gap: None,
		sealed_time: None,
		immutable: false,
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
				if result.timestamp.replace(value).is_some() {
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
			let bytes = data.slice_ref(bytes);
			if id == 0x0b {
				if immutable || std::mem::replace(&mut result.immutable, true) {
					return Err(Error::MalformedTrack);
				}
				let nested = list(&bytes, version, true).map_err(|_| Error::MalformedTrack)?;
				result.sealed_time = nested.time;
				result.time = result.time.or(nested.time);
				if let Some(gap) = nested.gap
					&& result.gap.replace(gap).is_some()
				{
					return Err(Error::MalformedTrack);
				}
			}
			Value::Bytes(bytes)
		};
		result.values.push(Property { id, value });
	}
	Ok(result)
}

/// Serialize `values` as `encoding` does, in ascending ID order. Equal IDs keep their order.
fn serialize<'a>(values: impl IntoIterator<Item = &'a Property>, encoding: Encoding) -> Result<Vec<u8>, Error> {
	let mut sorted: Vec<_> = values.into_iter().collect();
	sorted.sort_by_key(|property| property.id);
	let mut out = Vec::new();
	let mut encoder = Encoder::new(&mut out, family(encoding).into());
	let mut previous = 0;
	for property in sorted {
		encoder.varint(match encoding {
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

/// Encode `values` as the block of an object whose properties arrived as `encoding`, or
/// `None` without any.
///
/// Sorting a delta-encoded list, which arrives ascending, and dropping its Timestamp
/// never grows it, so the block fits wherever the publisher's list did.
pub(crate) fn block(
	values: &[Property],
	encoding: Encoding,
	delivery_timeout: bool,
) -> Result<Option<crate::model::object::Block>, Error> {
	if values.is_empty() {
		return Ok(None);
	}
	Ok(Some(crate::model::object::Block {
		bytes: serialize(values, encoding)?.into(),
		encoding,
		immutable: values.iter().any(|property| property.id == 0x0b),
		delivery_timeout,
	}))
}

/// What a received [`Block`](crate::model::object::Block) holds beside its bytes.
pub(crate) struct Checked {
	pub immutable: bool,
	pub sealed_time: Option<u64>,
	pub gap: Option<u64>,
}

/// Validate a block received as `encoding`, as moq-transport ingress validates a list.
///
/// Its mutable Timestamp travels beside it, so one inside is malformed, as is an empty
/// block, which is sent as none.
pub(crate) fn check(bytes: &Bytes, encoding: Encoding) -> Result<Checked, Error> {
	if bytes.is_empty() || bytes.len() > MAX_BLOCK {
		return Err(Error::MalformedTrack);
	}
	let parsed = list(bytes, family(encoding), false)?;
	if parsed.timestamp.is_some() {
		return Err(Error::MalformedTrack);
	}
	Ok(Checked {
		immutable: parsed.immutable,
		sealed_time: parsed.sealed_time,
		gap: parsed.gap,
	})
}

/// Every property but the mutable Timestamp, decoded, for tests to inspect.
#[cfg(test)]
pub(crate) fn listed(properties: &Properties) -> Vec<Property> {
	properties
		.block
		.as_ref()
		.map_or_else(Vec::new, |block| values(block).unwrap())
}

/// Decode a block's properties, whose values borrow its bytes.
pub(crate) fn values(block: &crate::model::object::Block) -> Result<Vec<Property>, Error> {
	Ok(list(&block.bytes, family(block.encoding), false)?.values)
}

pub(crate) fn read(data: &Bytes, version: Version, timescale: Option<Timescale>) -> Result<Decoded, Error> {
	let parsed = list(data, version, false)?;
	let properties = Properties {
		timestamp: parsed.timestamp,
		timescale: timescale.filter(|_| parsed.time.is_some()),
		sealed_time: parsed.sealed_time,
		block: block(&parsed.values, encoding(version), has_delivery_timeout(version))?,
	};
	let timestamp = match (parsed.time, timescale) {
		(Some(value), Some(timescale)) => Some(Timestamp::new(value, timescale).map_err(|_| Error::MalformedTrack)?),
		_ => None,
	};
	Ok(Decoded {
		properties,
		timestamp,
		gap: parsed.gap,
	})
}

/// The mutable Timestamp `properties` carries, in the `timescale` an egress declares.
///
/// An untimed ingress forwards its Timestamps as is. A timed one to an egress that
/// declares no units drops them, as a plain frame there carries none. Rewriting or
/// dropping an immutable Timestamp would break it, so that is refused.
pub(crate) fn egress_time(properties: &Properties, timescale: Option<Timescale>) -> Result<Option<u64>, Error> {
	let unstamped = properties.timescale.is_some() && timescale.is_none();
	let convert = match (properties.timescale, timescale) {
		(Some(from), Some(to)) if from != to => Some((from, to)),
		_ => None,
	};
	if properties.sealed_time.is_some() && (convert.is_some() || unstamped) {
		return Err(Error::Unsupported);
	}
	properties
		.timestamp
		.filter(|_| !unstamped)
		.map(|value| match convert {
			Some((from, to)) => Timestamp::new(value, from)
				.and_then(|timestamp| timestamp.convert(to))
				.map(|timestamp| timestamp.value())
				.map_err(|_| Error::Unsupported),
			None => Ok(value),
		})
		.transpose()
}

/// A draft that serializes properties in the `encoding` family. Any draft in the family
/// parses its keys and integers the same way.
pub(super) fn family(encoding: Encoding) -> Version {
	match encoding {
		Encoding::Absolute => Version::Draft14,
		Encoding::DeltaQuic => Version::Draft16,
		Encoding::Delta { seven: false } => Version::Draft17,
		Encoding::Delta { seven: true } => Version::Draft18,
	}
}

/// Serialize relayed properties for `version`, with Timestamps in the `timescale` the
/// egress declares, if any.
pub(crate) fn write(properties: &Properties, version: Version, timescale: Option<Timescale>) -> Result<Vec<u8>, Error> {
	let target = encoding(version);
	let timestamp = egress_time(properties, timescale)?;
	let timestamp = timestamp.map(|value| Property {
		id: 0x10,
		value: Value::Int(value),
	});
	let Some(block) = &properties.block else {
		return serialize(&timestamp, target);
	};
	if block.immutable && block.encoding != target {
		return Err(Error::Unsupported);
	}
	// Across draft-19, forwarding 0x06 would give it a different meaning.
	// A mutable one is dropped; a sealed one is refused.
	let flipped = block.delivery_timeout != has_delivery_timeout(version);
	// The common case: the block is already what this draft would send.
	if timestamp.is_none() && !flipped && block.encoding == target {
		return Ok(block.bytes.to_vec());
	}
	let values = values(block)?;
	if flipped && sealed_key(&values, version, 0x06)? {
		return Err(Error::Unsupported);
	}
	serialize(
		values
			.iter()
			.filter(|property| property.id != 0x06 || !flipped)
			.chain(&timestamp),
		target,
	)
}

/// Whether the immutable block among `values` holds a property with `id`.
fn sealed_key(values: &[Property], version: Version, id: u64) -> Result<bool, Error> {
	let Some(sealed) = values.iter().find(|property| property.id == 0x0b) else {
		return Ok(false);
	};
	let Value::Bytes(bytes) = &sealed.value else {
		return Err(Error::MalformedTrack);
	};
	let nested = list(bytes, version, true)?;
	Ok(nested.values.iter().any(|property| property.id == id))
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
				assert_eq!(decoded.properties.sealed_time, None, "{version}");
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
			listed(&decoded.properties)[0].value,
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
			assert_eq!(listed(&again.properties).last().unwrap().value, Value::Int(7));

			// An egress that declares no units drops it, as a plain frame there has none.
			let written = write(&decoded.properties, version, None).unwrap();
			let again = read(&Bytes::from(written), version, None).unwrap();
			assert_eq!(again.properties.timestamp, None, "{version}");
			assert_eq!(listed(&again.properties).len(), 1, "{version}");
			assert_eq!(listed(&again.properties)[0].value, Value::Int(7));
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
		assert_eq!(decoded.properties.sealed_time, None);
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
