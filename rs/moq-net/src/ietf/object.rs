use bytes::Bytes;

use super::Version;
use crate::{
	Error, Timescale, Timestamp,
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

/// Whether the Object Status `status` is defined, 0 being Normal.
pub(crate) fn status_defined(status: u64) -> bool {
	matches!(status, 0 | END_OF_GROUP | END_OF_TRACK)
}

/// Refuse the properties of an object with a frame on a track in `timescale` if they
/// leave no room for the Timestamp egress restates.
pub(crate) fn check_frame(properties: &Properties, timescale: Option<Timescale>) -> Result<(), Error> {
	// A list at the limit with a small Timestamp cannot take back a larger one on egress.
	match properties.heap() > max_block(timescale) as u64 {
		true => Err(Error::Unsupported),
		false => Ok(()),
	}
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
	pub timestamp: Option<Timestamp>,
	pub gap: Option<u64>,
	pub inflated: Option<u64>,
}

/// The largest properties block an object may carry, which a relay forwards unchanged.
pub(crate) const MAX_BLOCK: usize = super::group::MAX_OBJECT_EXTENSIONS;

/// The largest block a relay holds for an object on a track in `timescale`.
///
/// On a timed track, egress adds the frame's Timestamp to the block, so the block leaves
/// room for the largest one any draft encodes. Refusing a larger block on ingress keeps
/// every egress within [`MAX_BLOCK`].
pub(crate) fn max_block(timescale: Option<Timescale>) -> usize {
	if timescale.is_none() {
		return MAX_BLOCK;
	}
	// A Timestamp's value is a QUIC varint at most, and its key, a delta of at most 0x10,
	// only shrinks the key of the property after it.
	let largest = [Version::Draft18]
		.into_iter()
		.map(|version| {
			let form = version.into();
			crate::coding::varint::size(0x10, form).unwrap() + crate::coding::varint::size((1 << 62) - 1, form).unwrap()
		})
		.max()
		.unwrap();
	MAX_BLOCK - largest
}

struct List {
	/// In arrival order, without the mutable Timestamp. Values borrow the input.
	values: Vec<Property>,
	timestamp: Option<u64>,
	time: Option<u64>,
	gap: Option<u64>,
	sealed_time: Option<u64>,
	inflated: Option<u64>,
	sealed_inflated: Option<u64>,
	immutable: bool,
}

/// Parse a property list. `lift` takes the mutable Timestamp out of `values`, as a timed
/// track's frame holds it.
fn list(data: &Bytes, version: Version, immutable: bool, lift: bool) -> Result<List, Error> {
	let mut decoder = Decoder::new(data, version.into());
	let mut result = List {
		values: Vec::new(),
		timestamp: None,
		time: None,
		gap: None,
		sealed_time: None,
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
			if id == 0x3e && result.gap.replace(value).is_some() {
				return Err(Error::MalformedTrack);
			}
			if id == super::flate::INFLATED && result.inflated.replace(value).is_some() {
				return Err(Error::MalformedTrack);
			}
			if id == 0x10 {
				result.time = Some(value);
				if result.timestamp.replace(value).is_some() {
					return Err(Error::MalformedTrack);
				}
				if lift {
					continue;
				}
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
				let nested = list(&bytes, version, true, true).map_err(|_| Error::MalformedTrack)?;
				result.sealed_time = nested.time;
				result.sealed_inflated = nested.inflated;
				if let Some(size) = nested.inflated
					&& result.inflated.replace(size).is_some()
				{
					return Err(Error::MalformedTrack);
				}
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

/// Encode `values` as the block of an object whose properties arrived on `version`, or
/// `None` without any.
///
/// Sorting a delta-encoded list, which arrives ascending, and dropping its Timestamp
/// never grows it, so the block fits wherever the publisher's list did.
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
	pub sealed_time: Option<Timestamp>,
	pub gap: Option<u64>,
}

/// Validate a block received on a track in `timescale` that a `version` publisher
/// published, as moq-transport ingress validates a list, for an object that is `framed` or
/// a frameless status.
///
/// A framed object's mutable Timestamp on a timed track is its frame's, so one inside is
/// malformed, as is an empty block, which is sent as none. Absolute IDs must ascend, as
/// delta ones do, since egress may forward the block unchanged; Immutable Properties stay
/// as the publisher sent them.
pub(crate) fn check(
	bytes: &Bytes,
	version: Version,
	timescale: Option<Timescale>,
	framed: bool,
) -> Result<Checked, Error> {
	let lifted = timescale.filter(|_| framed);
	if bytes.is_empty() || bytes.len() > max_block(lifted) {
		return Err(Error::MalformedTrack);
	}
	let parsed = list(bytes, version, false, lifted.is_some())?;
	if lifted.is_some() && parsed.timestamp.is_some() {
		return Err(Error::MalformedTrack);
	}
	if !parsed.values.is_sorted_by_key(|property| property.id) {
		return Err(Error::MalformedTrack);
	}
	Ok(Checked {
		immutable: parsed.immutable,
		sealed_time: timed(parsed.sealed_time, timescale)?,
		gap: parsed.gap,
	})
}

/// A Timestamp `value` in the track's `timescale`, or none on an untimed track.
fn timed(value: Option<u64>, timescale: Option<Timescale>) -> Result<Option<Timestamp>, Error> {
	value
		.zip(timescale)
		.map(|(value, timescale)| Timestamp::new(value, timescale).map_err(|_| Error::MalformedTrack))
		.transpose()
}

/// Restate an object's known size declaration, preserving sealed declarations verbatim.
pub(super) fn compression(data: Vec<u8>, version: Version, inflated: Option<u64>) -> Result<Vec<u8>, Error> {
	if data.is_empty() && inflated.is_none() {
		return Ok(data);
	}
	let mut parsed = list(&Bytes::from(data), version, false, false)?;
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
	Ok(list(&block.bytes, version, false, false)?.values)
}

/// Read an object's property list on a track in `timescale`. A timed track's frame holds
/// the object's time, its mutable Timestamp or else a sealed one, so the block does not.
///
/// The caller applies [`check_frame`] once the object has a frame.
pub(crate) fn read(data: &Bytes, version: Version, timescale: Option<Timescale>) -> Result<Decoded, Error> {
	decode(data, version, timescale, true)
}

/// Read the property list of an object without a frame, such as an END_OF_GROUP status,
/// on a track in `timescale`. No frame holds its time, so a mutable Timestamp stays in the
/// block, as on an untimed track.
pub(crate) fn unframed(data: &Bytes, version: Version, timescale: Option<Timescale>) -> Result<Properties, Error> {
	let decoded = decode(data, version, timescale, false)?;
	if decoded.inflated.is_some() {
		return Err(Error::MalformedTrack);
	}
	Ok(decoded.properties)
}

fn decode(data: &Bytes, version: Version, timescale: Option<Timescale>, framed: bool) -> Result<Decoded, Error> {
	let lifted = timescale.filter(|_| framed);
	let mut parsed = list(data, version, false, lifted.is_some())?;
	if framed {
		parsed.values.retain(|p| p.id != super::flate::INFLATED);
	}
	let properties = Properties {
		sealed_time: timed(parsed.sealed_time, timescale)?,
		block: block(&parsed.values, version)?,
	};
	if properties.heap() > MAX_BLOCK as u64 {
		return Err(Error::Unsupported);
	}
	Ok(Decoded {
		properties,
		timestamp: timed(parsed.time, timescale)?,
		gap: parsed.gap,
		inflated: parsed.inflated,
	})
}

/// The mutable Timestamp to send for an object whose `frame` holds its time, on a timed
/// track, in the `timescale` an egress declares.
///
/// None when the sealed Timestamp already says it, or when the egress declares no units,
/// as a plain frame there carries none. Rewriting or dropping a sealed Timestamp would
/// break it, so that is refused.
pub(crate) fn egress_time(
	properties: &Properties,
	frame: Option<Timestamp>,
	timescale: Option<Timescale>,
) -> Result<Option<u64>, Error> {
	let Some(frame) = frame else {
		return Ok(None);
	};
	if let Some(sealed) = properties.sealed_time {
		if timescale != Some(sealed.scale()) {
			return Err(Error::Unsupported);
		}
		if frame.convert(sealed.scale()).is_ok_and(|frame| frame == sealed) {
			return Ok(None);
		}
	}
	timescale
		.map(|timescale| {
			frame
				.convert(timescale)
				.map(|time| time.value())
				.map_err(|_| Error::Unsupported)
		})
		.transpose()
}

/// Serialize relayed properties for `version`, with the `frame`'s time as the mutable
/// Timestamp in the `timescale` the egress declares, if any.
///
/// `source` is the draft the track's publisher spoke, which serialized the block. A track
/// without one carries no properties.
pub(crate) fn write(
	properties: &Properties,
	source: Option<Version>,
	frame: Option<Timestamp>,
	version: Version,
	timescale: Option<Timescale>,
) -> Result<Vec<u8>, Error> {
	let timestamp = egress_time(properties, frame, timescale)?;
	let timestamp = timestamp.map(|value| Property {
		id: 0x10,
		value: Value::Int(value),
	});
	let Some(block) = &properties.block else {
		return serialize(&timestamp, version);
	};
	let source = source.ok_or(Error::MalformedTrack)?;
	// Between draft-18 and draft-22, forwarding 0x06 would give it a different meaning.
	// A mutable one is dropped; a sealed one is refused.
	let flipped = has_delivery_timeout(source) != has_delivery_timeout(version);
	// The common case: the block is already what this draft would send.
	if timestamp.is_none() && !flipped {
		return Ok(block.bytes.to_vec());
	}
	let values = values(block, source)?;
	if flipped && sealed_key(&values, source, 0x06)? {
		return Err(Error::Unsupported);
	}
	let out = serialize(
		values
			.iter()
			.filter(|property| property.id != 0x06 || !flipped)
			.chain(&timestamp),
		version,
	)?;
	// Ingress leaves room for the Timestamp, but another draft's varints can be wider, so a
	// block re-encoded for it may not fit: refused, as the receiver would refuse it.
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
	let nested = list(bytes, version, true, true)?;
	Ok(nested.values.iter().any(|property| property.id == id))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn key_six_is_opaque_on_every_draft() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			for bytes in [
				Bytes::from_static(&[0x06, 42]),
				Bytes::from_static(&[0x0b, 2, 0x06, 42]),
			] {
				let decoded = read(&bytes, version, Some(Timescale::MILLI)).unwrap();
				assert_eq!(decoded.timestamp, None, "{version}");
				assert_eq!(decoded.properties.sealed_time, None, "{version}");
				for timescale in [Some(Timescale::MICRO), None] {
					assert_eq!(
						write(
							&decoded.properties,
							Some(version),
							decoded.timestamp,
							version,
							timescale
						)
						.unwrap(),
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
			listed(&decoded.properties, Version::Draft22)[0].value,
			Value::Bytes(Bytes::from_static(&[42]))
		);
	}

	#[test]
	fn immutable_timestamp_is_read_without_adding_an_outer_property() {
		let bytes = Bytes::from_static(&[0x0b, 2, 0x10, 42]);
		let decoded = read(&bytes, Version::Draft22, Some(Timescale::MILLI)).unwrap();
		assert_eq!(decoded.timestamp, Some(Timestamp::from_millis(42).unwrap()));
		assert_eq!(decoded.properties.sealed_time, decoded.timestamp);
		let milli = Some(Timescale::MILLI);
		assert_eq!(
			write(
				&decoded.properties,
				Some(Version::Draft22),
				decoded.timestamp,
				Version::Draft18,
				milli
			)
			.unwrap(),
			bytes
		);
		// Its units cannot follow the egress's, or vanish with them, without rewriting the block.
		for timescale in [Some(Timescale::MICRO), None] {
			assert!(matches!(
				write(
					&decoded.properties,
					Some(Version::Draft22),
					decoded.timestamp,
					Version::Draft22,
					timescale
				),
				Err(Error::Unsupported)
			));
		}
		// An untimed ingress has no units to lose.
		let untimed = read(&bytes, Version::Draft22, None).unwrap();
		assert_eq!(
			write(
				&untimed.properties,
				Some(Version::Draft22),
				untimed.timestamp,
				Version::Draft18,
				None
			)
			.unwrap(),
			bytes
		);
	}

	/// The cache keeps a timed track in its own units, so a relayed Timestamp must leave
	/// in the units the egress declares, not the ones it arrived in.
	#[test]
	fn mutable_timestamps_follow_the_egress_units() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let bytes = Bytes::from_static(&[0x10, 42, 0x20, 7]);
			let decoded = read(&bytes, version, Some(Timescale::MILLI)).unwrap();
			let written = write(
				&decoded.properties,
				Some(version),
				decoded.timestamp,
				version,
				Some(Timescale::MICRO),
			)
			.unwrap();
			let again = read(&Bytes::from(written), version, Some(Timescale::MICRO)).unwrap();
			let micros = Timestamp::new(42_000, Timescale::MICRO).unwrap();
			assert_eq!(again.timestamp, Some(micros), "{version}");
			assert_eq!(listed(&again.properties, version).last().unwrap().value, Value::Int(7));

			// An egress that declares no units drops it, as a plain frame there has none.
			let written = write(&decoded.properties, Some(version), decoded.timestamp, version, None).unwrap();
			let again = read(&Bytes::from(written), version, None).unwrap();
			assert_eq!(again.timestamp, None, "{version}");
			assert_eq!(listed(&again.properties, version).len(), 1, "{version}");
			assert_eq!(listed(&again.properties, version)[0].value, Value::Int(7));
			// An untimed ingress forwards it untouched.
			let untimed = read(&bytes, version, None).unwrap();
			assert_eq!(
				write(&untimed.properties, Some(version), untimed.timestamp, version, None).unwrap(),
				bytes
			);
		}
	}

	/// A timed track's frame holds an object's time, so egress restates it as the mutable
	/// Timestamp unless the sealed one already says it. Nothing else holds a second copy that
	/// could disagree.
	#[test]
	fn the_frame_holds_a_timed_objects_time() {
		let milli = Some(Timescale::MILLI);
		let at = |millis| Some(Timestamp::from_millis(millis).unwrap());

		// Kept properties that say nothing of time still leave with the frame's.
		let written = write(&Properties::default(), None, at(500), Version::Draft22, milli).unwrap();
		let again = read(&Bytes::from(written), Version::Draft22, milli).unwrap();
		assert_eq!(again.timestamp, at(500));

		// A mutable Timestamp beside a different sealed one is restated from the frame.
		let both = Bytes::from_static(&[0x0b, 2, 0x10, 42, 0x10 - 0x0b, 50]);
		let decoded = read(&both, Version::Draft22, milli).unwrap();
		assert_eq!((decoded.timestamp, decoded.properties.sealed_time), (at(50), at(42)));
		assert_eq!(
			write(
				&decoded.properties,
				Some(Version::Draft22),
				decoded.timestamp,
				Version::Draft22,
				milli
			)
			.unwrap(),
			both
		);

		// One equal to the sealed one adds nothing to it.
		let equal = Bytes::from_static(&[0x0b, 2, 0x10, 42, 0x10 - 0x0b, 42]);
		let decoded = read(&equal, Version::Draft22, milli).unwrap();
		assert_eq!(
			write(
				&decoded.properties,
				Some(Version::Draft22),
				decoded.timestamp,
				Version::Draft22,
				milli
			)
			.unwrap(),
			[0x0b, 2, 0x10, 42]
		);
	}

	/// A timed block leaves room for the Timestamp egress restates from the frame, so even
	/// the largest one leaves within [`MAX_BLOCK`]. A byte more is refused at ingress.
	#[test]
	fn a_timed_block_leaves_room_for_its_timestamp() {
		let milli = Some(Timescale::MILLI);
		let limit = max_block(milli);
		assert_eq!(limit, MAX_BLOCK - 10, "a one-byte key and a nine-byte value");
		// A draft-22 block of `len` bytes: 0x20 = 1 and zero deltas, one value taking two
		// bytes for an odd length. Its list adds a Timestamp in front.
		let block = |len: usize| {
			let mut block = vec![0x20, 1];
			if len % 2 == 1 {
				block.extend([0, 0x80, 0xc8]);
			}
			while block.len() < len {
				block.extend([0, 1]);
			}
			assert_eq!(block.len(), len);
			Bytes::from(block)
		};
		let list = |len: usize| {
			let mut list = vec![0x10, 1, 0x10];
			list.extend(&block(len)[1..]);
			Bytes::from(list)
		};
		let decoded = read(&list(limit), Version::Draft22, milli).unwrap();
		assert_eq!(decoded.properties.heap(), limit as u64);
		assert!(check_frame(&decoded.properties, milli).is_ok());
		let latest = Timestamp::new((1 << 61) - 1, Timescale::MILLI).unwrap();
		let written = write(
			&decoded.properties,
			Some(Version::Draft22),
			Some(latest),
			Version::Draft22,
			milli,
		)
		.unwrap();
		assert_eq!(written.len(), MAX_BLOCK);

		let over = read(&list(limit + 1), Version::Draft22, milli).unwrap();
		assert!(matches!(check_frame(&over.properties, milli), Err(Error::Unsupported)));
		// An untimed track has no Timestamp to restate, so its block may fill the limit.
		let untimed = read(&list(MAX_BLOCK - 2), Version::Draft22, None).unwrap();
		assert!(check_frame(&untimed.properties, None).is_ok());

		// The same bound on a block arriving over the lite hop.
		let version = Version::Draft22;
		assert!(check(&block(limit), version, milli, true).is_ok());
		assert!(check(&block(limit + 1), version, milli, true).is_err());
		assert!(check(&block(limit + 1), version, None, true).is_ok());
	}

	/// A status object has no frame to hold its time, so its mutable Timestamp stays in the
	/// block on a timed track, where a framed object's is lifted out.
	#[test]
	fn a_status_object_keeps_its_timestamp() {
		let milli = Some(Timescale::MILLI);
		let list = Bytes::from_static(&[0x10, 7, 0x3e, 1]);
		let framed = read(&list, Version::Draft18, milli).unwrap();
		assert_eq!(framed.timestamp, Some(Timestamp::new(7, Timescale::MILLI).unwrap()));
		let status = unframed(&list, Version::Draft18, milli).unwrap();
		assert_eq!(
			write(&status, Some(Version::Draft18), None, Version::Draft18, None).unwrap(),
			list.to_vec()
		);

		// Over the lite hop, only a status block may hold it.
		let block = status.block.unwrap().bytes;
		assert!(check(&block, Version::Draft18, milli, false).is_ok());
		assert!(check(&block, Version::Draft18, milli, true).is_err());
	}

	/// Draft-22 assigns 0x06 to SUBGROUP_DELIVERY_TIMEOUT, so it cannot cross that boundary.
	#[test]
	fn key_six_follows_its_draft() {
		let bytes = Bytes::from_static(&[0x06, 42, 0x1a, 7]);
		let milli = Some(Timescale::MILLI);
		let decoded = read(&bytes, Version::Draft22, milli).unwrap();
		assert_eq!(decoded.timestamp, None);
		for timescale in [Some(Timescale::MICRO), None] {
			assert_eq!(
				write(
					&decoded.properties,
					Some(Version::Draft22),
					decoded.timestamp,
					Version::Draft22,
					timescale
				)
				.unwrap(),
				bytes
			);
		}

		// Between draft-18 and draft-22 neither reading survives: a mutable one is dropped.
		let modern = Bytes::from_static(&[0x20, 7]);
		assert_eq!(
			write(
				&decoded.properties,
				Some(Version::Draft22),
				decoded.timestamp,
				Version::Draft18,
				milli
			)
			.unwrap(),
			modern
		);
		let legacy = read(&bytes, Version::Draft18, milli).unwrap();
		assert_eq!(
			write(
				&legacy.properties,
				Some(Version::Draft18),
				legacy.timestamp,
				Version::Draft22,
				milli
			)
			.unwrap(),
			modern
		);

		// A sealed one is refused.
		let sealed = Bytes::from_static(&[0x0b, 2, 0x06, 42]);
		let decoded = read(&sealed, Version::Draft22, milli).unwrap();
		assert_eq!(decoded.properties.sealed_time, None);
		assert_eq!(
			write(
				&decoded.properties,
				Some(Version::Draft22),
				decoded.timestamp,
				Version::Draft22,
				None
			)
			.unwrap(),
			sealed
		);
		assert!(matches!(
			write(
				&decoded.properties,
				Some(Version::Draft22),
				decoded.timestamp,
				Version::Draft18,
				milli
			),
			Err(Error::Unsupported)
		));
		let legacy = read(&sealed, Version::Draft18, milli).unwrap();
		assert!(matches!(
			write(
				&legacy.properties,
				Some(Version::Draft18),
				legacy.timestamp,
				Version::Draft22,
				milli
			),
			Err(Error::Unsupported)
		));
	}

	#[test]
	fn immutable_lists_validate_nested_blocks_and_integer_forms() {
		for bytes in [&[0x0b, 2, 0x0b, 0][..], &[0x0b, 0, 0, 0][..], &[0x0b, 1, 2][..]] {
			assert!(read(&Bytes::copy_from_slice(bytes), Version::Draft22, None).is_err());
		}
		// Draft-18 permits this nonminimal seven-byte value.
		let bytes = Bytes::from_static(&[0x0b, 8, 2, 0xfc, 0, 0, 0, 0, 0, 1]);
		let decoded = read(&bytes, Version::Draft18, None).unwrap();
		assert_eq!(
			write(
				&decoded.properties,
				Some(Version::Draft18),
				decoded.timestamp,
				Version::Draft22,
				None
			)
			.unwrap(),
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
			.create_track("data", crate::track::Info::default().with_compress(true))
			.unwrap();
		let group = track.append_group().unwrap();
		let header = crate::frame::Header {
			info: crate::frame::Info {
				size: 1,
				timestamp: None,
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
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let unknown = property(0x8800, 7);
			let wire = compression(serialize([&unknown], version).unwrap(), version, Some(123)).unwrap();
			let decoded = read(&wire.into(), version, None).unwrap();
			assert_eq!(decoded.inflated, Some(123));
			assert_eq!(listed(&decoded.properties, version), [unknown]);
			assert_eq!(inflated(Some(0), true).unwrap(), Some(0));
			assert!(inflated(None, true).is_err());
			assert!(inflated(Some(0), false).is_err());
		}
	}
	#[test]
	fn a_status_cannot_declare_an_inflated_size() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let size = property(super::super::flate::INFLATED, 0);
			for value in [
				size.clone(),
				Property {
					id: 0x0b,
					value: Value::Bytes(serialize([&size], version).unwrap().into()),
				},
			] {
				let wire = serialize([&value], version).unwrap();
				assert!(matches!(
					unframed(&wire.into(), version, None),
					Err(Error::MalformedTrack)
				));
			}
		}
	}

	#[test]
	fn sealed_sizes_are_forwarded_or_refused_without_rewriting() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let sealed = Property {
				id: 0x0b,
				value: Value::Bytes(
					serialize([&property(super::super::flate::INFLATED, 23)], version)
						.unwrap()
						.into(),
				),
			};
			let wire = serialize([&sealed], version).unwrap();
			let decoded = read(&wire.clone().into(), version, None).unwrap();
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
			assert!(read(&repeated.into(), version, None).is_err());
		}
	}
}
