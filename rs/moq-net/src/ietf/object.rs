use bytes::Bytes;

use super::Version;
use crate::{
	Error,
	coding::{Decoder, Encoder},
	model::object::{Properties, Property, Value},
};

/// Draft-22 assigns property 0x06 to SUBGROUP_DELIVERY_TIMEOUT.
pub(crate) fn has_delivery_timeout(version: Version) -> bool {
	!matches!(version, Version::Draft18)
}

/// Object status: no object at or past this location in the group exists.
pub(crate) const END_OF_GROUP: u64 = 0x3;
/// Object status: no object at or past this location exists (every implemented draft).
pub(crate) const END_OF_TRACK: u64 = 0x4;

/// Object property: how many Group IDs just below this object's never exist.
const PRIOR_GROUP_ID_GAP: u64 = 0x3c;
/// Object property: how many Object IDs just below this object's never exist.
const PRIOR_OBJECT_ID_GAP: u64 = 0x3e;

/// Whether the Object Status `status` is defined, 0 being Normal.
pub(crate) fn status_defined(status: u64) -> bool {
	matches!(status, 0 | END_OF_GROUP | END_OF_TRACK)
}

/// Refuse properties on a status object, such as END_OF_GROUP, which may not carry any.
#[cfg(feature = "async")]
pub(super) fn check_status(properties: &Properties) -> Result<(), Error> {
	match properties.is_empty() {
		true => Ok(()),
		false => Err(Error::Session(crate::SessionError::ProtocolViolation)),
	}
}

pub(crate) struct Decoded {
	pub properties: Properties,
	/// The Prior Object ID Gap.
	pub gap: Option<u64>,
	/// The Prior Group ID Gap, which also stays in the block.
	pub group_gap: Option<u64>,
	pub inflated: Option<u64>,
}

/// The largest properties block an object may carry, which a relay forwards unchanged.
pub(crate) const MAX_BLOCK: usize = super::group::MAX_OBJECT_EXTENSIONS;

struct List {
	/// In arrival order. Values borrow the input.
	values: Vec<Property>,
	gap: Option<u64>,
	group_gap: Option<u64>,
	inflated: Option<u64>,
	sealed_inflated: Option<u64>,
	immutable: bool,
}

/// Parse a property list, `immutable` when it is the one Immutable Properties holds.
fn list(data: &Bytes, version: Version, immutable: bool) -> Result<List, Error> {
	let mut decoder = Decoder::new(data, version.into());
	let mut result = List {
		values: Vec::new(),
		gap: None,
		group_gap: None,
		inflated: None,
		sealed_inflated: None,
		immutable: false,
	};
	let mut previous = 0u64;
	while !decoder.is_empty() {
		let key = decoder.varint()?;
		let id = previous.checked_add(key).ok_or(Error::MalformedTrack)?;
		previous = id;
		let value = if id % 2 == 0 {
			let value = decoder.varint()?;
			let repeated = match id {
				PRIOR_OBJECT_ID_GAP => result.gap.replace(value).is_some(),
				PRIOR_GROUP_ID_GAP => result.group_gap.replace(value).is_some(),
				super::flate::INFLATED => result.inflated.replace(value).is_some(),
				_ => false,
			};
			if repeated {
				return Err(Error::MalformedTrack);
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
				result.sealed_inflated = nested.inflated;
				// Each appears once, inside the Immutable Properties or outside them.
				for (outer, inner) in [
					(&mut result.inflated, nested.inflated),
					(&mut result.gap, nested.gap),
					(&mut result.group_gap, nested.group_gap),
				] {
					if let Some(value) = inner
						&& outer.replace(value).is_some()
					{
						return Err(Error::MalformedTrack);
					}
				}
			}
			Value::Bytes(bytes)
		};
		result.values.push(Property { id, value });
	}
	Ok(result)
}

/// Serialize `values` as `version` does, in ascending ID order. Equal IDs keep their order.
fn serialize<'a>(values: impl IntoIterator<Item = &'a Property>, version: Version) -> Result<Vec<u8>, Error> {
	let mut sorted: Vec<_> = values.into_iter().collect();
	sorted.sort_by_key(|property| property.id);
	let mut out = Vec::new();
	let mut encoder = Encoder::new(&mut out, version.into());
	let mut previous = 0;
	for property in sorted {
		encoder.varint(property.id - previous)?;
		previous = property.id;
		match &property.value {
			Value::Int(value) => encoder.varint(*value)?,
			Value::Bytes(value) => encoder.bytes(value)?,
		}
	}
	Ok(out)
}

/// The properties our own group's first object carries: a Prior Group ID Gap of `gap`.
pub(crate) fn group_gap(gap: u64, version: Version) -> Result<Vec<u8>, Error> {
	let property = Property {
		id: PRIOR_GROUP_ID_GAP,
		value: Value::Int(gap),
	};
	serialize([&property], version)
}

/// Encode `values` as the block of an object whose properties arrived on `version`, or
/// `None` without any.
///
/// Sorting a delta-encoded list, which arrives ascending, never grows it, so the block
/// fits wherever the publisher's list did.
pub(crate) fn block(values: &[Property], version: Version) -> Result<Option<crate::model::object::Block>, Error> {
	if values.is_empty() {
		return Ok(None);
	}
	Ok(Some(crate::model::object::Block {
		bytes: serialize(values, version)?.into(),
		immutable: values.iter().any(|property| property.id == 0x0b),
	}))
}

/// What a received [`Block`](crate::model::object::Block) holds beside its bytes.
pub(crate) struct Checked {
	pub immutable: bool,
	pub gap: Option<u64>,
}

/// Validate a block a `version` publisher published, as moq-transport ingress validates a
/// list.
///
/// An empty block is malformed, since it is sent as none. Absolute IDs must ascend, as
/// delta ones do, since egress may forward the block unchanged; Immutable Properties stay
/// as the publisher sent them.
pub(crate) fn check(bytes: &Bytes, version: Version) -> Result<Checked, Error> {
	if bytes.is_empty() || bytes.len() > MAX_BLOCK {
		return Err(Error::MalformedTrack);
	}
	let parsed = list(bytes, version, false)?;
	if !parsed.values.is_sorted_by_key(|property| property.id) {
		return Err(Error::MalformedTrack);
	}
	Ok(Checked {
		immutable: parsed.immutable,
		gap: parsed.gap,
	})
}

/// Restate an object's known size declaration, preserving sealed declarations verbatim.
pub(super) fn compression(data: Vec<u8>, version: Version, inflated: Option<u64>) -> Result<Vec<u8>, Error> {
	if data.is_empty() && inflated.is_none() {
		return Ok(data);
	}
	let mut parsed = list(&Bytes::from(data), version, false)?;
	if let Some(sealed) = parsed.sealed_inflated {
		return match inflated == Some(sealed) {
			true => serialize(&parsed.values, version),
			false => Err(Error::Unsupported),
		};
	}
	parsed.values.retain(|p| p.id != super::flate::INFLATED);
	if let Some(size) = inflated {
		parsed.values.push(Property {
			id: super::flate::INFLATED,
			value: Value::Int(size),
		});
	}
	let out = serialize(&parsed.values, version)?;
	if out.len() > MAX_BLOCK {
		return Err(Error::Unsupported);
	}
	Ok(out)
}

/// A compressed frame must declare its inflated size; a plain frame must not.
pub(super) fn inflated(size: Option<u64>, compressed: bool) -> Result<Option<u64>, Error> {
	if size.is_some() != compressed {
		return Err(Error::MalformedTrack);
	}
	Ok(size)
}

/// Every property a block from a `version` publisher holds, decoded, for tests to inspect.
#[cfg(test)]
pub(crate) fn listed(properties: &Properties, version: Version) -> Vec<Property> {
	properties
		.block
		.as_ref()
		.map_or_else(Vec::new, |block| values(block, version).unwrap())
}

/// Decode the properties of a block from a `version` publisher, whose values borrow its bytes.
fn values(block: &crate::model::object::Block, version: Version) -> Result<Vec<Property>, Error> {
	Ok(list(&block.bytes, version, false)?.values)
}

/// Read an object's property list. Every property but the inflated size, which the frame
/// holds, stays in the block, a Timestamp included: nothing reads time from an object.
pub(crate) fn read(data: &Bytes, version: Version) -> Result<Decoded, Error> {
	decode(data, version, true)
}

/// Read the property list of an object without a frame, such as an END_OF_GROUP status.
pub(crate) fn unframed(data: &Bytes, version: Version) -> Result<Properties, Error> {
	let decoded = decode(data, version, false)?;
	if decoded.inflated.is_some() {
		return Err(Error::MalformedTrack);
	}
	Ok(decoded.properties)
}

fn decode(data: &Bytes, version: Version, framed: bool) -> Result<Decoded, Error> {
	let mut parsed = list(data, version, false)?;
	if framed {
		parsed.values.retain(|p| p.id != super::flate::INFLATED);
	}
	let properties = Properties {
		block: block(&parsed.values, version)?,
	};
	if properties.heap() > MAX_BLOCK as u64 {
		return Err(Error::Unsupported);
	}
	Ok(Decoded {
		properties,
		gap: parsed.gap,
		group_gap: parsed.group_gap,
		inflated: parsed.inflated,
	})
}

/// Serialize relayed properties for `version`.
///
/// `source` is the draft the track's publisher spoke, which serialized the block. A track
/// without one carries no properties.
pub(crate) fn write(properties: &Properties, source: Option<Version>, version: Version) -> Result<Vec<u8>, Error> {
	let Some(block) = &properties.block else {
		return Ok(Vec::new());
	};
	let source = source.ok_or(Error::MalformedTrack)?;
	// Between draft-18 and draft-22, forwarding 0x06 would give it a different meaning.
	// A mutable one is dropped; a sealed one is refused.
	if has_delivery_timeout(source) == has_delivery_timeout(version) {
		// The common case: the block is already what this draft would send.
		return Ok(block.bytes.to_vec());
	}
	let values = values(block, source)?;
	if sealed_key(&values, source, 0x06)? {
		return Err(Error::Unsupported);
	}
	let out = serialize(values.iter().filter(|property| property.id != 0x06), version)?;
	// Another draft's varints can be wider, so a block re-encoded for it may not fit:
	// refused, as the receiver would refuse it.
	if out.len() > MAX_BLOCK {
		return Err(Error::Unsupported);
	}
	Ok(out)
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

	const DRAFTS: [Version; 4] = [Version::Draft18, Version::Draft20, Version::Draft22, Version::Draft23];

	#[test]
	fn key_six_is_opaque_on_every_draft() {
		for version in DRAFTS {
			for bytes in [
				Bytes::from_static(&[0x06, 42]),
				Bytes::from_static(&[0x0b, 2, 0x06, 42]),
			] {
				let decoded = read(&bytes, version).unwrap();
				assert_eq!(
					write(&decoded.properties, Some(version), version).unwrap(),
					bytes,
					"{version}"
				);
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
		let decoded = read(&received, Version::Draft22).unwrap();
		drop(received);
		assert!(dropped.load(Ordering::Relaxed));
		assert_eq!(
			listed(&decoded.properties, Version::Draft22)[0].value,
			Value::Bytes(Bytes::from_static(&[42]))
		);
	}

	/// Nothing reads time from an object: a Timestamp, mutable or sealed, is a property
	/// like any other, which passes between moq-transport peers byte for byte.
	#[test]
	fn a_timestamp_passes_through_untouched() {
		for version in DRAFTS {
			for bytes in [
				Bytes::from_static(&[0x10, 42, 0x20, 7]),
				Bytes::from_static(&[0x0b, 2, 0x10, 42, 0x10 - 0x0b, 50]),
			] {
				let decoded = read(&bytes, version).unwrap();
				assert_eq!(
					write(&decoded.properties, Some(version), version).unwrap(),
					bytes,
					"{version}"
				);
				let status = unframed(&bytes, version).unwrap();
				assert_eq!(write(&status, Some(version), version).unwrap(), bytes, "{version}");
				assert!(check(&bytes, version).is_ok());
			}
		}
	}

	/// The Prior Group ID Gap is read wherever it is, and stays in the block, which a relay
	/// forwards unchanged. One object may name it once.
	#[test]
	fn the_prior_group_id_gap_is_read_and_kept() {
		for version in DRAFTS {
			let outer = Bytes::from(group_gap(2, version).unwrap());
			let decoded = read(&outer, version).unwrap();
			assert_eq!(decoded.group_gap, Some(2), "{version}");
			assert_eq!(decoded.gap, None, "{version}");
			assert_eq!(write(&decoded.properties, Some(version), version).unwrap(), outer);

			let sealed = Bytes::from_static(&[0x0b, 2, 0x3c, 5]);
			assert_eq!(read(&sealed, version).unwrap().group_gap, Some(5), "{version}");

			for repeated in [&[0x3c, 1, 0, 1][..], &[0x0b, 2, 0x3c, 5, 0x3c - 0x0b, 5][..]] {
				assert!(matches!(
					read(&Bytes::copy_from_slice(repeated), version),
					Err(Error::MalformedTrack)
				));
			}
		}
	}

	/// Draft-22 assigns 0x06 to SUBGROUP_DELIVERY_TIMEOUT, so it cannot cross that boundary.
	#[test]
	fn key_six_follows_its_draft() {
		let bytes = Bytes::from_static(&[0x06, 42, 0x1a, 7]);
		let decoded = read(&bytes, Version::Draft22).unwrap();
		assert_eq!(
			write(&decoded.properties, Some(Version::Draft22), Version::Draft22).unwrap(),
			bytes
		);

		// Between draft-18 and draft-22 neither reading survives: a mutable one is dropped.
		let modern = Bytes::from_static(&[0x20, 7]);
		assert_eq!(
			write(&decoded.properties, Some(Version::Draft22), Version::Draft18).unwrap(),
			modern
		);
		let legacy = read(&bytes, Version::Draft18).unwrap();
		assert_eq!(
			write(&legacy.properties, Some(Version::Draft18), Version::Draft22).unwrap(),
			modern
		);

		// A sealed one is refused.
		let sealed = Bytes::from_static(&[0x0b, 2, 0x06, 42]);
		let decoded = read(&sealed, Version::Draft22).unwrap();
		assert_eq!(
			write(&decoded.properties, Some(Version::Draft22), Version::Draft22).unwrap(),
			sealed
		);
		assert!(matches!(
			write(&decoded.properties, Some(Version::Draft22), Version::Draft18),
			Err(Error::Unsupported)
		));
		let legacy = read(&sealed, Version::Draft18).unwrap();
		assert!(matches!(
			write(&legacy.properties, Some(Version::Draft18), Version::Draft22),
			Err(Error::Unsupported)
		));
	}

	#[test]
	fn immutable_lists_validate_nested_blocks_and_integer_forms() {
		for bytes in [&[0x0b, 2, 0x0b, 0][..], &[0x0b, 0, 0, 0][..], &[0x0b, 1, 2][..]] {
			assert!(read(&Bytes::copy_from_slice(bytes), Version::Draft22).is_err());
		}
		// Draft-18 permits this nonminimal seven-byte value.
		let bytes = Bytes::from_static(&[0x0b, 8, 2, 0xfc, 0, 0, 0, 0, 0, 1]);
		let decoded = read(&bytes, Version::Draft18).unwrap();
		assert_eq!(
			write(&decoded.properties, Some(Version::Draft18), Version::Draft22).unwrap(),
			bytes
		);
	}
}

#[cfg(test)]
mod compression_tests {
	use super::*;
	#[test]
	fn inflated_size_is_admitted_before_any_inflate() {
		let broadcast = crate::broadcast::Info::default().produce();
		let track = broadcast
			.create_track(
				"data",
				crate::track::Info::default().with_compression(crate::track::Compression::Deflate),
			)
			.unwrap();
		let group = track.append_next().unwrap();
		let header = crate::frame::Header {
			info: crate::frame::Info {
				size: 1,
				timestamp: crate::Timestamp::unscaled(group.start),
			},
			fields: None,
			inflated: inflated(Some(crate::group::MAX_CACHE_BYTES + 1), true).unwrap(),
		};
		assert!(matches!(group.create_object(header), Err(Error::FrameTooLarge)));
	}

	fn property(id: u64, value: u64) -> Property {
		Property {
			id,
			value: Value::Int(value),
		}
	}
	#[test]
	fn sizes_are_known_and_unknown_properties_survive() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22, Version::Draft23] {
			let unknown = property(0x8800, 7);
			let wire = compression(serialize([&unknown], version).unwrap(), version, Some(123)).unwrap();
			let decoded = read(&wire.into(), version).unwrap();
			assert_eq!(decoded.inflated, Some(123));
			assert_eq!(listed(&decoded.properties, version), [unknown]);
			assert_eq!(inflated(Some(0), true).unwrap(), Some(0));
			assert!(inflated(None, true).is_err());
			assert!(inflated(Some(0), false).is_err());
		}
	}
	#[test]
	fn a_status_cannot_declare_an_inflated_size() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22, Version::Draft23] {
			let size = property(super::super::flate::INFLATED, 0);
			for value in [
				size.clone(),
				Property {
					id: 0x0b,
					value: Value::Bytes(serialize([&size], version).unwrap().into()),
				},
			] {
				let wire = serialize([&value], version).unwrap();
				assert!(matches!(unframed(&wire.into(), version), Err(Error::MalformedTrack)));
			}
		}
	}

	#[test]
	fn sealed_sizes_are_forwarded_or_refused_without_rewriting() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22, Version::Draft23] {
			let sealed = Property {
				id: 0x0b,
				value: Value::Bytes(
					serialize([&property(super::super::flate::INFLATED, 23)], version)
						.unwrap()
						.into(),
				),
			};
			let wire = serialize([&sealed], version).unwrap();
			let decoded = read(&wire.clone().into(), version).unwrap();
			assert_eq!(decoded.inflated, Some(23));
			assert!(super::super::flate::egress_object(&decoded.properties, Some(version), true).is_ok());
			assert!(matches!(
				super::super::flate::egress_object(&decoded.properties, Some(version), false),
				Err(Error::Unsupported)
			));
			assert_eq!(compression(wire.clone(), version, Some(23)).unwrap(), wire);
			assert!(matches!(
				compression(wire.clone(), version, None),
				Err(Error::Unsupported)
			));
			assert!(matches!(
				compression(wire.clone(), version, Some(24)),
				Err(Error::Unsupported)
			));
			let repeated = serialize([&sealed, &property(super::super::flate::INFLATED, 23)], version).unwrap();
			assert!(read(&repeated.into(), version).is_err());
		}
	}
}
