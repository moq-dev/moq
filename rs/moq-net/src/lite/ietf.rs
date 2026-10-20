//! The `ietf` extension (draft-lcurley-moq-ietf): the moq-transport object state a relay
//! preserves, carried across a lite-07 hop when both SETUPs offer it.
//!
//! Every field here is crate-private relay metadata (see `model::object`). Plain readers
//! never see it, and a session without the extension sends none of it.

use std::sync::Arc;

use bytes::Bytes;

use crate::{
	Error, Timescale,
	coding::{DecodeError, Decoder, EncodeError, Encoder, Writer},
	group,
	model::object::{Encoding, Fields, Properties, Property, Value},
};

use super::{Message, Version};

/// The largest properties block a peer may make us buffer, as for a moq-transport object.
pub(crate) const MAX_PROPERTIES: usize = 64 * 1024;

// Group State bits.
const RELAYED: u64 = 0x1;
const FIRST_OBJECT: u64 = 0x2;
const EXTENSIONS: u64 = 0x4;
const END_OF_GROUP: u64 = 0x8;

// Datagram State bits, after RELAYED.
const OBJECT_ID: u64 = 0x2;
const PRIORITY: u64 = 0x4;
const DATAGRAM_END: u64 = 0x8;
const STATUS: u64 = 0x10;

// Properties Flags: the low bit, then the Immutable Properties encoding above it.
const DELIVERY_TIMEOUT: u64 = 0x1;

/// Property IDs the model reads.
const TIMESTAMP: u64 = 0x10;
const IMMUTABLE: u64 = 0x0b;
const GAP: u64 = 0x3e;

/// The Group State a stream sends for its group's `wire` state.
///
/// `whole` when the stream starts at the copy's first frame, so FIRST_OBJECT still holds,
/// and `capped` when it stops before the group may end, so its FIN cannot end the group.
pub(crate) fn encode_state(wire: Option<&group::Wire>, whole: bool, capped: bool) -> u64 {
	let Some(wire) = wire else {
		return 0;
	};
	let mut state = RELAYED;
	if wire.first_object && whole {
		state |= FIRST_OBJECT;
	}
	if wire.extensions {
		state |= EXTENSIONS;
	}
	if wire.has_end && !capped {
		state |= END_OF_GROUP;
	}
	state
}

/// The relay state a received Group State describes, or `None` for a plain group.
///
/// The first object's ID is learned from the first record, so it starts at `frame_start`.
pub(crate) fn decode_state(state: u64, frame_start: u64) -> Result<Option<group::Wire>, Error> {
	if state == 0 {
		return Ok(None);
	}
	if state & RELAYED == 0 || state & !(RELAYED | FIRST_OBJECT | EXTENSIONS | END_OF_GROUP) != 0 {
		return Err(Error::ProtocolViolation);
	}
	Ok(Some(group::Wire {
		sparse: false,
		first_object: state & FIRST_OBJECT != 0,
		first_id: frame_start,
		extensions: state & EXTENSIONS != 0,
		has_end: state & END_OF_GROUP != 0,
		fetchable: true,
		last: None,
	}))
}

/// The first varint of an object record: the Object ID delta from `prior`, or the absolute
/// ID for the first record, with the END_OF_GROUP status in the low bit.
pub(crate) fn encode_object(prior: Option<u64>, id: u64, end: bool) -> Result<u64, Error> {
	let delta = match prior {
		None => id,
		Some(prior) => id
			.checked_sub(prior)
			.and_then(|step| step.checked_sub(1))
			.ok_or(Error::MalformedTrack)?,
	};
	if delta > u64::MAX >> 1 {
		return Err(Error::BoundsExceeded(crate::coding::BoundsExceeded));
	}
	Ok(delta << 1 | u64::from(end))
}

/// The Object ID and END_OF_GROUP status an object record's first varint names.
pub(crate) fn decode_object(prior: Option<u64>, code: u64) -> Result<(u64, bool), Error> {
	let delta = code >> 1;
	let id = match prior {
		None => Some(delta),
		Some(prior) => prior.checked_add(1).and_then(|next| next.checked_add(delta)),
	};
	Ok((id.ok_or(Error::ProtocolViolation)?, code & 1 != 0))
}

fn encoding_code(encoding: Option<Encoding>) -> u64 {
	match encoding {
		None => 0,
		Some(Encoding::Absolute) => 1,
		Some(Encoding::DeltaQuic) => 2,
		Some(Encoding::Delta { seven: false }) => 3,
		Some(Encoding::Delta { seven: true }) => 4,
	}
}

fn encode_flags(delivery_timeout: bool, immutable: Option<Encoding>) -> u64 {
	u64::from(delivery_timeout) | encoding_code(immutable) << 1
}

fn decode_flags(flags: u64) -> Result<(bool, Option<Encoding>), DecodeError> {
	let encoding = match flags >> 1 {
		0 => None,
		1 => Some(Encoding::Absolute),
		2 => Some(Encoding::DeltaQuic),
		3 => Some(Encoding::Delta { seven: false }),
		4 => Some(Encoding::Delta { seven: true }),
		_ => return Err(DecodeError::InvalidValue),
	};
	Ok((flags & DELIVERY_TIMEOUT != 0, encoding))
}

/// Encode each property, with absolute IDs: an even ID has an integer value, an odd one bytes.
fn encode_list<'a>(w: &mut Encoder<'_>, values: impl IntoIterator<Item = &'a Property>) -> Result<(), EncodeError> {
	for property in values {
		w.varint(property.id)?;
		match (&property.value, property.id % 2) {
			(Value::Int(value), 0) => w.varint(*value)?,
			(Value::Bytes(value), 1) => w.bytes(value)?,
			_ => return Err(EncodeError::InvalidState),
		}
	}
	Ok(())
}

/// Decode properties to the end of `r`, in the order they arrived.
fn decode_list(r: &mut Decoder<'_>) -> Result<Vec<Property>, DecodeError> {
	let mut values = Vec::new();
	while !r.is_empty() {
		let id = r.varint()?;
		let value = match id % 2 {
			0 => Value::Int(r.varint()?),
			// A small cached property must not pin the reader's larger receive buffer.
			_ => Value::Bytes(Bytes::copy_from_slice(r.bytes()?)),
		};
		values.push(Property { id, value });
	}
	Ok(values)
}

/// Check the one Immutable Properties block against the encoding the flags declare.
fn immutable<'a>(values: &'a [Property], encoding: Option<Encoding>) -> Result<Option<&'a Bytes>, DecodeError> {
	let mut sealed = values.iter().filter(|property| property.id == IMMUTABLE);
	let block = match sealed.next().map(|property| &property.value) {
		Some(Value::Bytes(bytes)) => Some(bytes),
		Some(Value::Int(_)) => unreachable!("odd IDs decode as bytes"),
		None => None,
	};
	if sealed.next().is_some() || block.is_some() != encoding.is_some() {
		return Err(DecodeError::InvalidValue);
	}
	Ok(block)
}

/// Encode an object's properties as a block body, empty when it has none.
///
/// `timestamp` replaces its mutable Timestamp, already in the units the receiver's track
/// declares (see [`crate::ietf::object::egress_time`]).
pub(crate) fn encode_properties(
	properties: &Properties,
	timestamp: Option<u64>,
	version: Version,
) -> Result<Vec<u8>, EncodeError> {
	let mut out = Vec::new();
	if timestamp.is_none() && properties.values.is_empty() {
		return Ok(out);
	}
	let w = &mut Encoder::new(&mut out, version.into());
	w.varint(encode_flags(properties.delivery_timeout, properties.immutable))?;
	let timestamp = timestamp.map(|value| Property {
		id: TIMESTAMP,
		value: Value::Int(value),
	});
	encode_list(w, timestamp.iter().chain(&properties.values))?;
	Ok(out)
}

/// An object's decoded properties and the Prior Object ID Gap they mark, if any.
pub(crate) struct Decoded {
	pub properties: Properties,
	pub gap: Option<u64>,
}

/// Decode an object's properties block body, whose Timestamps are in `timescale`, the
/// units the track declares.
pub(crate) fn decode_properties(
	block: &[u8],
	timescale: Option<Timescale>,
	version: Version,
) -> Result<Decoded, DecodeError> {
	let mut properties = Properties::default();
	if block.is_empty() {
		return Ok(Decoded { properties, gap: None });
	}
	let r = &mut Decoder::new(block, version.into());
	(properties.delivery_timeout, properties.immutable) = decode_flags(r.varint()?)?;
	let mut values = decode_list(r)?;

	let mut timestamps = values.iter().filter(|property| property.id == TIMESTAMP);
	properties.timestamp = match timestamps.next() {
		Some(Property {
			value: Value::Int(value),
			..
		}) => Some(*value),
		Some(_) => unreachable!("even IDs decode as integers"),
		None => None,
	};
	if timestamps.next().is_some() {
		return Err(DecodeError::Duplicate);
	}
	values.retain(|property| property.id != TIMESTAMP);

	let mut gaps = values.iter().filter_map(|property| match property {
		Property {
			id: GAP,
			value: Value::Int(gap),
		} => Some(*gap),
		_ => None,
	});
	let mut gap = gaps.next();
	if gaps.next().is_some() {
		return Err(DecodeError::Duplicate);
	}
	if let (Some(block), Some(encoding)) = (immutable(&values, properties.immutable)?, properties.immutable) {
		let sealed = crate::ietf::object::sealed(block, encoding).map_err(|_| DecodeError::InvalidValue)?;
		properties.sealed_time = sealed.time;
		if let Some(sealed) = sealed.gap
			&& gap.replace(sealed).is_some()
		{
			return Err(DecodeError::Duplicate);
		}
	}
	properties.values = values;
	stamp(&mut properties, timescale);
	Ok(Decoded { properties, gap })
}

/// Record that `properties` carry their Timestamps in `timescale`, the units the track
/// declares, when they carry any.
pub(crate) fn stamp(properties: &mut Properties, timescale: Option<Timescale>) {
	properties.timescale = timescale.filter(|_| properties.timestamp.is_some() || properties.sealed_time);
}

/// A group stream's object records on egress, sent before each frame of a relayed group.
pub(crate) struct Records {
	/// The previous record's Object ID, `None` before the first.
	prior: Option<u64>,
	/// The units the track declares, which mutable Timestamps convert to.
	timescale: Option<Timescale>,
	version: Version,
}

impl Records {
	/// Buffer the Group State for a stream serving `group` from its current index, and
	/// return the records its frames need, `None` for a plain group.
	///
	/// `capped` when the stream stops before the group may end. Writes nothing unless
	/// the session negotiated the extension.
	pub fn start<W: crate::transport::poll::SendStream>(
		writer: &mut Writer<W, Version>,
		ietf: bool,
		group: &group::Consumer,
		capped: bool,
		timescale: Option<Timescale>,
		version: Version,
	) -> Result<Option<Self>, Error> {
		if !ietf {
			return Ok(None);
		}
		let wire = group.wire();
		let whole = group.index() <= group.first_frame();
		writer.buffer_varint(encode_state(wire.as_ref(), whole, capped))?;
		Ok(wire.map(|_| Self {
			prior: None,
			timescale,
			version,
		}))
	}

	/// Buffer the record for the frame at `index`, ahead of its timestamp and size.
	pub fn frame<W: crate::transport::poll::SendStream>(
		&mut self,
		writer: &mut Writer<W, Version>,
		index: u64,
		fields: Option<&Fields>,
	) -> Result<(), Error> {
		self.record(writer, fields.map_or(index, |fields| fields.id), fields, false)
	}

	/// Buffer the END_OF_GROUP record once every frame is written, if the group has one.
	pub fn end<W: crate::transport::poll::SendStream>(
		&mut self,
		writer: &mut Writer<W, Version>,
		group: &group::Consumer,
	) -> Result<(), Error> {
		// A stream capped short of the last frame stops before the end.
		if group.index() < group.frame_count() as u64 {
			return Ok(());
		}
		match group.end_object() {
			Some(end) => self.record(writer, end.id, Some(&end), true),
			None => Ok(()),
		}
	}

	fn record<W: crate::transport::poll::SendStream>(
		&mut self,
		writer: &mut Writer<W, Version>,
		id: u64,
		fields: Option<&Fields>,
		end: bool,
	) -> Result<(), Error> {
		writer.buffer_varint(encode_object(self.prior, id, end)?)?;
		self.prior = Some(id);
		let block = match fields {
			Some(fields) => {
				let timestamp = crate::ietf::object::egress_time(&fields.properties, self.timescale)?;
				encode_properties(&fields.properties, timestamp, self.version)?
			}
			None => Vec::new(),
		};
		writer.buffer_varint(block.len() as u64)?;
		writer.buffer_raw(&block);
		Ok(())
	}
}

/// The moq-transport Track Properties a relay forwards, sent on the Track Stream right
/// after TRACK_INFO when the extension is negotiated.
#[derive(Clone, Debug, Default)]
pub struct TrackProperties(pub Option<Arc<crate::model::object::TrackProperties>>);

impl Message for TrackProperties {
	fn encode_msg(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		let Some(properties) = &self.0 else {
			w.varint(0)?;
			return w.varint(0);
		};
		w.varint(encode_flags(properties.delivery_timeout, properties.immutable))?;
		w.varint(properties.sealed.len() as u64)?;
		for id in &properties.sealed {
			w.varint(*id)?;
		}
		encode_list(w, &properties.values)
	}

	fn decode_msg(r: &mut Decoder<'_>, _: Version) -> Result<Self, DecodeError> {
		let (delivery_timeout, encoding) = decode_flags(r.varint()?)?;
		let count = r.varint()?;
		// Every ID takes at least a byte, so this bounds the allocation by the message.
		if count > r.remaining() as u64 {
			return Err(DecodeError::Short);
		}
		let sealed = (0..count).map(|_| r.varint()).collect::<Result<Vec<_>, _>>()?;
		let values = decode_list(r)?;
		if immutable(&values, encoding)?.is_none() && !sealed.is_empty() {
			return Err(DecodeError::InvalidValue);
		}
		if values.is_empty() {
			return Ok(Self(None));
		}
		Ok(Self(Some(Arc::new(crate::model::object::TrackProperties {
			values,
			immutable: encoding,
			sealed,
			delivery_timeout,
		}))))
	}
}

/// Encode a datagram's relay state after its Group Sequence, or a plain one's as 0.
pub(crate) fn encode_datagram(
	w: &mut Encoder<'_>,
	wire: Option<&crate::model::datagram::Wire>,
	version: Version,
) -> Result<(), EncodeError> {
	let Some(wire) = wire else {
		return w.varint(0);
	};
	let mut state = RELAYED;
	for (set, bit) in [
		(wire.object_id.is_some(), OBJECT_ID),
		(wire.priority.is_some(), PRIORITY),
		(wire.end_of_group, DATAGRAM_END),
		(wire.status.is_some(), STATUS),
	] {
		if set {
			state |= bit;
		}
	}
	w.varint(state)?;
	if let Some(id) = wire.object_id {
		w.varint(id)?;
	}
	if let Some(priority) = wire.priority {
		w.u8(priority);
	}
	if let Some(status) = wire.status {
		w.varint(status)?;
	}
	w.bytes(&encode_properties(&wire.properties, wire.properties.timestamp, version)?)
}

/// Decode a datagram's relay state. Its properties' units are the track's, which the
/// caller [`stamp`]s.
pub(crate) fn decode_datagram(
	r: &mut Decoder<'_>,
	version: Version,
) -> Result<Option<crate::model::datagram::Wire>, DecodeError> {
	let state = r.varint()?;
	if state == 0 {
		return Ok(None);
	}
	if state & RELAYED == 0 || state & !(RELAYED | OBJECT_ID | PRIORITY | DATAGRAM_END | STATUS) != 0 {
		return Err(DecodeError::InvalidValue);
	}
	let object_id = (state & OBJECT_ID != 0).then(|| r.varint()).transpose()?;
	let priority = (state & PRIORITY != 0).then(|| r.u8()).transpose()?;
	let status = (state & STATUS != 0).then(|| r.varint()).transpose()?;
	let block = r.bytes()?;
	let properties = decode_properties(block, None, version)?.properties;
	Ok(Some(crate::model::datagram::Wire {
		object_id,
		priority,
		end_of_group: state & DATAGRAM_END != 0,
		status,
		properties,
	}))
}

#[cfg(test)]
mod tests {
	use super::*;

	const V: Version = Version::Lite07;

	#[test]
	fn object_codes_round_trip() {
		for (prior, id, end) in [(None, 0, false), (None, 7, true), (Some(3), 4, false), (Some(3), 9, true)] {
			let code = encode_object(prior, id, end).unwrap();
			assert_eq!(decode_object(prior, code).unwrap(), (id, end));
		}
		// A plain frame after its predecessor costs one byte.
		assert_eq!(encode_object(Some(4), 5, false).unwrap(), 0);
		assert!(matches!(encode_object(Some(4), 4, false), Err(Error::MalformedTrack)));
		assert!(decode_object(Some(u64::MAX), 0).is_err());
	}

	#[test]
	fn group_state_round_trips() {
		let wire = group::Wire {
			sparse: true,
			first_object: true,
			first_id: 9,
			extensions: true,
			has_end: true,
			fetchable: false,
			last: Some(12),
		};
		let state = encode_state(Some(&wire), true, false);
		let decoded = decode_state(state, 2).unwrap().unwrap();
		assert!(decoded.first_object && decoded.extensions && decoded.has_end);
		assert_eq!(decoded.first_id, 2);
		assert!(decoded.fetchable && !decoded.sparse);

		let partial = decode_state(encode_state(Some(&wire), false, true), 0).unwrap().unwrap();
		assert!(!partial.first_object && !partial.has_end);

		assert_eq!(encode_state(None, true, false), 0);
		assert!(decode_state(0, 0).unwrap().is_none());
		assert!(decode_state(FIRST_OBJECT, 0).is_err(), "state without RELAYED");
		assert!(decode_state(RELAYED | 0x10, 0).is_err(), "unknown bit");
	}

	#[test]
	fn properties_round_trip_with_their_provenance() {
		// An immutable block in the draft-16 family holding a gap of 2 (0x3e) and a timestamp.
		let mut sealed = Vec::new();
		let e = &mut Encoder::new(&mut sealed, crate::ietf::Version::Draft16.into());
		for value in [0x10u64, 77, 0x3e - 0x10, 2] {
			e.varint(value).unwrap();
		}
		let properties = Properties {
			timestamp: Some(1_000),
			values: vec![
				Property {
					id: 0x06,
					value: Value::Int(5),
				},
				Property {
					id: IMMUTABLE,
					value: Value::Bytes(sealed.into()),
				},
			],
			immutable: Some(Encoding::DeltaQuic),
			timescale: Some(Timescale::MILLI),
			sealed_time: true,
			delivery_timeout: true,
		};
		let block = encode_properties(&properties, properties.timestamp, V).unwrap();
		let decoded = decode_properties(&block, Some(Timescale::MILLI), V).unwrap();
		assert_eq!(decoded.properties, properties);
		assert_eq!(decoded.gap, Some(2));

		// None at all is an empty block.
		assert!(encode_properties(&Properties::default(), None, V).unwrap().is_empty());
		let empty = decode_properties(&[], Some(Timescale::MILLI), V).unwrap();
		assert_eq!(empty.properties, Properties::default());
	}

	#[test]
	fn properties_refuse_a_mismatched_immutable_encoding() {
		let properties = Properties {
			values: vec![Property {
				id: IMMUTABLE,
				value: Value::Bytes(Bytes::new()),
			}],
			..Default::default()
		};
		let block = encode_properties(&properties, None, V).unwrap();
		assert!(decode_properties(&block, None, V).is_err(), "no encoding declared");

		let declared = Properties {
			immutable: Some(Encoding::Absolute),
			..Default::default()
		};
		let block = encode_properties(&declared, Some(1), V).unwrap();
		assert!(decode_properties(&block, None, V).is_err(), "encoding without a block");
	}

	#[test]
	fn track_properties_round_trip() {
		let properties = crate::model::object::TrackProperties {
			values: vec![
				Property {
					id: 0x41,
					value: Value::Bytes(Bytes::from_static(b"opaque")),
				},
				Property {
					id: IMMUTABLE,
					value: Value::Bytes(Bytes::from_static(&[0x02, 0x01])),
				},
			],
			immutable: Some(Encoding::Delta { seven: true }),
			sealed: vec![0x02],
			delivery_timeout: true,
		};
		let msg = TrackProperties(Some(Arc::new(properties.clone())));
		let buf = crate::coding::Encode::encode_bytes(&msg, V).unwrap();
		let decoded = <TrackProperties as crate::coding::Decode<Version>>::decode(&mut Decoder::new(&buf, V.into()), V)
			.unwrap();
		assert_eq!(decoded.0.as_deref(), Some(&properties));

		let none = crate::coding::Encode::encode_bytes(&TrackProperties(None), V).unwrap();
		let decoded =
			<TrackProperties as crate::coding::Decode<Version>>::decode(&mut Decoder::new(&none, V.into()), V).unwrap();
		assert!(decoded.0.is_none());
	}

	#[test]
	fn datagram_state_round_trips() {
		let wire = crate::model::datagram::Wire {
			object_id: Some(3),
			priority: Some(9),
			end_of_group: true,
			status: Some(3),
			properties: Properties::default(),
		};
		let mut buf = Vec::new();
		encode_datagram(&mut Encoder::new(&mut buf, V.into()), Some(&wire), V).unwrap();
		let decoded = decode_datagram(&mut Decoder::new(&buf, V.into()), V).unwrap();
		assert_eq!(decoded, Some(wire));

		let mut plain = Vec::new();
		encode_datagram(&mut Encoder::new(&mut plain, V.into()), None, V).unwrap();
		assert_eq!(plain, [0]);
		assert_eq!(decode_datagram(&mut Decoder::new(&plain, V.into()), V).unwrap(), None);
	}
}
