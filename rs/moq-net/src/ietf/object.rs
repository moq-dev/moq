use bytes::Bytes;

use super::Version;
use crate::{
	Error, Timescale, Timestamp,
	coding::{Decoder, Encoder, Form},
	model::object::{Encoding, Properties, Property, Value},
};

pub(crate) fn encoding(version: Version) -> Encoding {
	match version {
		Version::Draft14 | Version::Draft15 => Encoding::Draft14,
		Version::Draft16 => Encoding::Draft16,
		Version::Draft17 => Encoding::Draft17,
		Version::Draft18 => Encoding::Draft18,
		_ => Encoding::Draft19,
	}
}

/// Whether lists in encodings `a` and `b` are the same bytes: the same keys and varints.
pub(crate) fn same_bytes(a: Encoding, b: Encoding) -> bool {
	let form = |encoding| (encoding == Encoding::Draft14, Form::from(family(encoding)));
	form(a) == form(b)
}

/// Whether property 0x06 means SUBGROUP_DELIVERY_TIMEOUT in a list from `encoding`.
pub(crate) fn delivery_timeout(encoding: Encoding) -> bool {
	has_delivery_timeout(family(encoding))
}

/// The drafts that serialize properties in the `family` encoding, one of which sent any
/// block claiming it.
pub(crate) fn drafts(family: Encoding) -> impl Iterator<Item = Version> {
	Version::ALL
		.into_iter()
		.filter(move |version| encoding(*version) == family)
}

/// Draft-19 assigns property 0x06 to SUBGROUP_DELIVERY_TIMEOUT.
pub(crate) fn has_delivery_timeout(version: Version) -> bool {
	!matches!(
		version,
		Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17 | Version::Draft18
	)
}

/// Object status: the object does not exist, defined by drafts 14 and 15 only.
pub(crate) const DOES_NOT_EXIST: u64 = 0x1;
/// Object status: no object at or past this location in the group exists.
pub(crate) const END_OF_GROUP: u64 = 0x3;
/// Object status: no object at or past this location exists (every implemented draft).
pub(crate) const END_OF_TRACK: u64 = 0x4;

/// Whether `version` defines the Object Status `status`, 0 being Normal.
pub(crate) fn status_defined(status: u64, version: Version) -> bool {
	match status {
		0 | END_OF_GROUP | END_OF_TRACK => true,
		DOES_NOT_EXIST => matches!(version, Version::Draft14 | Version::Draft15),
		_ => false,
	}
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

/// Only draft-14 lets a status object, such as END_OF_GROUP, carry properties.
#[cfg(feature = "async")]
pub(crate) fn status_has_properties(version: Version) -> bool {
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
	let largest = [Version::Draft14, Version::Draft16, Version::Draft17, Version::Draft18]
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
	immutable: bool,
}

/// Parse a property list. `lift` takes the mutable Timestamp out of `values`, as a timed
/// track's frame holds it.
fn list(data: &Bytes, version: Version, immutable: bool, lift: bool) -> Result<List, Error> {
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
			Encoding::Draft14 => key,
			_ => previous.checked_add(key).ok_or(Error::MalformedTrack)?,
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
			Encoding::Draft14 => property.id,
			_ => property.id - previous,
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
pub(crate) fn block(values: &[Property], encoding: Encoding) -> Result<Option<crate::model::object::Block>, Error> {
	if values.is_empty() {
		return Ok(None);
	}
	Ok(Some(crate::model::object::Block {
		bytes: serialize(values, encoding)?.into(),
		encoding,
		immutable: values.iter().any(|property| property.id == 0x0b),
	}))
}

/// What a received [`Block`](crate::model::object::Block) holds beside its bytes.
pub(crate) struct Checked {
	pub immutable: bool,
	pub sealed_time: Option<Timestamp>,
	pub gap: Option<u64>,
}

/// Validate a block received as `encoding` on a track in `timescale`, as moq-transport
/// ingress validates a list, for an object that is `framed` or a frameless status.
///
/// A framed object's mutable Timestamp on a timed track is its frame's, so one inside is
/// malformed, as is an empty block, which is sent as none. Absolute IDs must ascend, as
/// delta ones do, since egress may forward the block unchanged; Immutable Properties stay
/// as the publisher sent them.
pub(crate) fn check(
	bytes: &Bytes,
	encoding: Encoding,
	timescale: Option<Timescale>,
	framed: bool,
) -> Result<Checked, Error> {
	let lifted = timescale.filter(|_| framed);
	if bytes.is_empty() || bytes.len() > max_block(lifted) {
		return Err(Error::MalformedTrack);
	}
	let parsed = list(bytes, family(encoding), false, lifted.is_some())?;
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

/// Every property a block holds, decoded, for tests to inspect.
#[cfg(test)]
pub(crate) fn listed(properties: &Properties) -> Vec<Property> {
	properties
		.block
		.as_ref()
		.map_or_else(Vec::new, |block| values(block).unwrap())
}

/// Decode a block's properties, whose values borrow its bytes.
pub(crate) fn values(block: &crate::model::object::Block) -> Result<Vec<Property>, Error> {
	Ok(list(&block.bytes, family(block.encoding), false, false)?.values)
}

/// Read an object's property list on a track in `timescale`. A timed track's frame holds
/// the object's time, its mutable Timestamp or else a sealed one, so the block does not.
///
/// The caller applies [`check_frame`] once the object has a frame: a draft-14 status
/// object, whose list arrives before its status, may fill the whole block.
pub(crate) fn read(data: &Bytes, version: Version, timescale: Option<Timescale>) -> Result<Decoded, Error> {
	decode(data, version, timescale, true)
}

/// Read the property list of an object without a frame, such as an END_OF_GROUP status,
/// on a track in `timescale`. No frame holds its time, so a mutable Timestamp stays in the
/// block, as on an untimed track.
pub(crate) fn unframed(data: &Bytes, version: Version, timescale: Option<Timescale>) -> Result<Properties, Error> {
	Ok(decode(data, version, timescale, false)?.properties)
}

fn decode(data: &Bytes, version: Version, timescale: Option<Timescale>, framed: bool) -> Result<Decoded, Error> {
	let lifted = timescale.filter(|_| framed);
	let parsed = list(data, version, false, lifted.is_some())?;
	let properties = Properties {
		sealed_time: timed(parsed.sealed_time, timescale)?,
		block: block(&parsed.values, encoding(version))?,
	};
	if properties.heap() > MAX_BLOCK as u64 {
		return Err(Error::Unsupported);
	}
	Ok(Decoded {
		properties,
		timestamp: timed(parsed.time, timescale)?,
		gap: parsed.gap,
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

/// A draft that serializes properties in the `encoding` family. Any draft in the family
/// parses its keys and integers the same way.
pub(crate) fn family(encoding: Encoding) -> Version {
	match encoding {
		Encoding::Draft14 => Version::Draft14,
		Encoding::Draft16 => Version::Draft16,
		Encoding::Draft17 => Version::Draft17,
		Encoding::Draft18 => Version::Draft18,
		Encoding::Draft19 => Version::Draft19,
	}
}

/// Serialize relayed properties for `version`, with the `frame`'s time as the mutable
/// Timestamp in the `timescale` the egress declares, if any.
pub(crate) fn write(
	properties: &Properties,
	frame: Option<Timestamp>,
	version: Version,
	timescale: Option<Timescale>,
) -> Result<Vec<u8>, Error> {
	let target = encoding(version);
	let timestamp = egress_time(properties, frame, timescale)?;
	let timestamp = timestamp.map(|value| Property {
		id: 0x10,
		value: Value::Int(value),
	});
	let Some(block) = &properties.block else {
		return serialize(&timestamp, target);
	};
	if block.immutable && !same_bytes(block.encoding, target) {
		return Err(Error::Unsupported);
	}
	// Across draft-19, forwarding 0x06 would give it a different meaning.
	// A mutable one is dropped; a sealed one is refused.
	let flipped = delivery_timeout(block.encoding) != has_delivery_timeout(version);
	// The common case: the block is already what this draft would send.
	if timestamp.is_none() && !flipped && same_bytes(block.encoding, target) {
		return Ok(block.bytes.to_vec());
	}
	let values = values(block)?;
	if flipped && sealed_key(&values, version, 0x06)? {
		return Err(Error::Unsupported);
	}
	let out = serialize(
		values
			.iter()
			.filter(|property| property.id != 0x06 || !flipped)
			.chain(&timestamp),
		target,
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
						write(&decoded.properties, decoded.timestamp, version, timescale).unwrap(),
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
		assert_eq!(decoded.properties.sealed_time, decoded.timestamp);
		let milli = Some(Timescale::MILLI);
		assert_eq!(
			write(&decoded.properties, decoded.timestamp, Version::Draft18, milli).unwrap(),
			bytes
		);
		assert!(matches!(
			write(&decoded.properties, decoded.timestamp, Version::Draft17, milli),
			Err(Error::Unsupported)
		));
		// Its units cannot follow the egress's, or vanish with them, without rewriting the block.
		for timescale in [Some(Timescale::MICRO), None] {
			assert!(matches!(
				write(&decoded.properties, decoded.timestamp, Version::Draft18, timescale),
				Err(Error::Unsupported)
			));
		}
		// An untimed ingress has no units to lose.
		let untimed = read(&bytes, Version::Draft22, None).unwrap();
		assert_eq!(
			write(&untimed.properties, untimed.timestamp, Version::Draft18, None).unwrap(),
			bytes
		);
	}

	/// The cache keeps a timed track in its own units, so a relayed Timestamp must leave
	/// in the units the egress declares, not the ones it arrived in.
	#[test]
	fn mutable_timestamps_follow_the_egress_units() {
		for version in [Version::Draft18, Version::Draft22] {
			let bytes = Bytes::from_static(&[0x10, 42, 0x20, 7]);
			let decoded = read(&bytes, version, Some(Timescale::MILLI)).unwrap();
			let written = write(&decoded.properties, decoded.timestamp, version, Some(Timescale::MICRO)).unwrap();
			let again = read(&Bytes::from(written), version, Some(Timescale::MICRO)).unwrap();
			let micros = Timestamp::new(42_000, Timescale::MICRO).unwrap();
			assert_eq!(again.timestamp, Some(micros), "{version}");
			assert_eq!(listed(&again.properties).last().unwrap().value, Value::Int(7));

			// An egress that declares no units drops it, as a plain frame there has none.
			let written = write(&decoded.properties, decoded.timestamp, version, None).unwrap();
			let again = read(&Bytes::from(written), version, None).unwrap();
			assert_eq!(again.timestamp, None, "{version}");
			assert_eq!(listed(&again.properties).len(), 1, "{version}");
			assert_eq!(listed(&again.properties)[0].value, Value::Int(7));
			// An untimed ingress forwards it untouched.
			let untimed = read(&bytes, version, None).unwrap();
			assert_eq!(
				write(&untimed.properties, untimed.timestamp, version, None).unwrap(),
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
		let written = write(&Properties::default(), at(500), Version::Draft22, milli).unwrap();
		let again = read(&Bytes::from(written), Version::Draft22, milli).unwrap();
		assert_eq!(again.timestamp, at(500));

		// A mutable Timestamp beside a different sealed one is restated from the frame.
		let both = Bytes::from_static(&[0x0b, 2, 0x10, 42, 0x10 - 0x0b, 50]);
		let decoded = read(&both, Version::Draft22, milli).unwrap();
		assert_eq!((decoded.timestamp, decoded.properties.sealed_time), (at(50), at(42)));
		assert_eq!(
			write(&decoded.properties, decoded.timestamp, Version::Draft22, milli).unwrap(),
			both
		);

		// One equal to the sealed one adds nothing to it.
		let equal = Bytes::from_static(&[0x0b, 2, 0x10, 42, 0x10 - 0x0b, 42]);
		let decoded = read(&equal, Version::Draft22, milli).unwrap();
		assert_eq!(
			write(&decoded.properties, decoded.timestamp, Version::Draft22, milli).unwrap(),
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
		let latest = Timestamp::new((1 << 62) - 1, Timescale::MILLI).unwrap();
		let written = write(&decoded.properties, Some(latest), Version::Draft22, milli).unwrap();
		assert_eq!(written.len(), MAX_BLOCK);

		let over = read(&list(limit + 1), Version::Draft22, milli).unwrap();
		assert!(matches!(check_frame(&over.properties, milli), Err(Error::Unsupported)));
		// An untimed track has no Timestamp to restate, so its block may fill the limit.
		let untimed = read(&list(MAX_BLOCK - 2), Version::Draft22, None).unwrap();
		assert!(check_frame(&untimed.properties, None).is_ok());

		// The same bound on a block arriving over the lite hop.
		let encoding = Encoding::Draft19;
		assert!(check(&block(limit), encoding, milli, true).is_ok());
		assert!(check(&block(limit + 1), encoding, milli, true).is_err());
		assert!(check(&block(limit + 1), encoding, None, true).is_ok());
	}

	/// A status object has no frame to hold its time, so its mutable Timestamp stays in the
	/// block on a timed track, where a framed object's is lifted out.
	#[test]
	fn a_status_object_keeps_its_timestamp() {
		let milli = Some(Timescale::MILLI);
		let list = Bytes::from_static(&[0x10, 7, 0x3e, 1]);
		let framed = read(&list, Version::Draft14, milli).unwrap();
		assert_eq!(framed.timestamp, Some(Timestamp::new(7, Timescale::MILLI).unwrap()));
		let status = unframed(&list, Version::Draft14, milli).unwrap();
		assert_eq!(write(&status, None, Version::Draft14, None).unwrap(), list.to_vec());

		// Over the lite hop, only a status block may hold it.
		let block = status.block.unwrap().bytes;
		assert!(check(&block, Encoding::Draft14, milli, false).is_ok());
		assert!(check(&block, Encoding::Draft14, milli, true).is_err());
	}

	/// A draft-14 block crosses the lite hop in ascending order, as ingress sorts it, so
	/// one whose absolute IDs descend is malformed. Its Immutable Properties may not ascend.
	#[test]
	fn a_draft14_block_ascends() {
		let milli = Some(Timescale::MILLI);
		assert!(check(&Bytes::from_static(&[0x02, 1, 0x20, 1]), Encoding::Draft14, milli, true).is_ok());
		assert!(check(&Bytes::from_static(&[0x02, 1, 0x02, 2]), Encoding::Draft14, milli, true).is_ok());
		assert!(check(&Bytes::from_static(&[0x20, 1, 0x02, 1]), Encoding::Draft14, milli, true).is_err());
		let sealed = Bytes::from_static(&[0x02, 1, 0x0b, 4, 0x20, 1, 0x02, 1]);
		assert!(check(&sealed, Encoding::Draft14, milli, true).is_ok());
	}

	/// Draft-19 assigns 0x06 to SUBGROUP_DELIVERY_TIMEOUT, so it cannot cross that boundary.
	#[test]
	fn key_six_follows_its_draft() {
		let bytes = Bytes::from_static(&[0x06, 42, 0x1a, 7]);
		let milli = Some(Timescale::MILLI);
		let decoded = read(&bytes, Version::Draft22, milli).unwrap();
		assert_eq!(decoded.timestamp, None);
		for timescale in [Some(Timescale::MICRO), None] {
			assert_eq!(
				write(&decoded.properties, decoded.timestamp, Version::Draft19, timescale).unwrap(),
				bytes
			);
		}

		// Across draft-19 neither reading survives: a mutable one is dropped.
		let modern = Bytes::from_static(&[0x20, 7]);
		assert_eq!(
			write(&decoded.properties, decoded.timestamp, Version::Draft18, milli).unwrap(),
			modern
		);
		let legacy = read(&bytes, Version::Draft18, milli).unwrap();
		assert_eq!(
			write(&legacy.properties, legacy.timestamp, Version::Draft19, milli).unwrap(),
			modern
		);

		// A sealed one is refused.
		let sealed = Bytes::from_static(&[0x0b, 2, 0x06, 42]);
		let decoded = read(&sealed, Version::Draft22, milli).unwrap();
		assert_eq!(decoded.properties.sealed_time, None);
		assert_eq!(
			write(&decoded.properties, decoded.timestamp, Version::Draft22, None).unwrap(),
			sealed
		);
		assert!(matches!(
			write(&decoded.properties, decoded.timestamp, Version::Draft18, milli),
			Err(Error::Unsupported)
		));
		let legacy = read(&sealed, Version::Draft18, milli).unwrap();
		assert!(matches!(
			write(&legacy.properties, legacy.timestamp, Version::Draft19, milli),
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
		assert_eq!(
			write(&decoded.properties, decoded.timestamp, Version::Draft22, None).unwrap(),
			bytes
		);
		assert!(read(&bytes, Version::Draft17, None).is_err());
		assert!(matches!(
			write(&decoded.properties, decoded.timestamp, Version::Draft17, None),
			Err(Error::Unsupported)
		));
	}
}
