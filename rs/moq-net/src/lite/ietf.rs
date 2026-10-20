//! The `ietf` extension (draft-lcurley-moq-ietf): the moq-transport object state a relay
//! preserves, carried across a lite-07 hop when both SETUPs offer it.
//!
//! Every field here is crate-private relay metadata (see `model::object`). Plain readers
//! never see it, and a session without the extension sends none of it.

use std::sync::Arc;

use bytes::Bytes;

use crate::{
	Error, Timescale,
	coding::{DecodeError, Decoder, EncodeError, Encoder},
	group,
	model::object::{Encoding, Fields, Properties, Property, Value},
};

use super::{Message, Version};

/// The largest record body a peer may make us buffer: the largest block moq-transport
/// admits, beside a Flags byte and a Timestamp of at most 9 bytes.
pub(crate) const MAX_PROPERTIES: usize = crate::ietf::object::MAX_BLOCK + 10;

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

// Flags of a block and of TRACK_PROPERTIES: property 0x06 means SUBGROUP_DELIVERY_TIMEOUT.
const DELIVERY_TIMEOUT: u64 = 0x1;

/// Immutable Properties, whose keys TRACK_PROPERTIES derives its sealed list from.
const IMMUTABLE: u64 = 0x0b;

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

fn decode_encoding(code: u64) -> Result<Option<Encoding>, DecodeError> {
	Ok(match code {
		0 => None,
		1 => Some(Encoding::Absolute),
		2 => Some(Encoding::DeltaQuic),
		3 => Some(Encoding::Delta { seven: false }),
		4 => Some(Encoding::Delta { seven: true }),
		_ => return Err(DecodeError::InvalidValue),
	})
}

fn decode_flags(flags: u64) -> Result<(bool, Option<Encoding>), DecodeError> {
	Ok((flags & DELIVERY_TIMEOUT != 0, decode_encoding(flags >> 1)?))
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

/// Decode Track Properties to the end of `r`, in the order they arrived.
///
/// No more than moq-transport ingress keeps, which is all a relay forwards.
fn decode_list(r: &mut Decoder<'_>) -> Result<Vec<Property>, DecodeError> {
	let mut values = Vec::new();
	while !r.is_empty() {
		if values.len() as u64 >= crate::ietf::properties::MAX_PROPERTIES {
			return Err(DecodeError::TooMany);
		}
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
fn immutable(values: &[Property], encoding: Option<Encoding>) -> Result<Option<&Bytes>, DecodeError> {
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

/// The Flags bit saying a mutable Timestamp follows. Bit 0 is DELIVERY_TIMEOUT, and the
/// block's encoding sits above both.
const TIMESTAMPED: u64 = 0x2;

/// Encode an object's properties as a record body, empty when it has none.
///
/// `timestamp` replaces its mutable Timestamp, already in the units the receiver's track
/// declares (see [`crate::ietf::object::egress_time`]). The block is sent as it is held.
pub(crate) fn encode_properties(
	properties: &Properties,
	timestamp: Option<u64>,
	version: Version,
) -> Result<Vec<u8>, EncodeError> {
	let mut out = Vec::new();
	let block = properties.block.as_ref();
	if timestamp.is_none() && block.is_none() {
		return Ok(out);
	}
	let w = &mut Encoder::new(&mut out, version.into());
	let mut flags = block.map_or(0, |block| {
		u64::from(block.delivery_timeout) | encoding_code(Some(block.encoding)) << 2
	});
	if timestamp.is_some() {
		flags |= TIMESTAMPED;
	}
	w.varint(flags)?;
	if let Some(timestamp) = timestamp {
		w.varint(timestamp)?;
	}
	if let Some(block) = block {
		w.slice(&block.bytes);
	}
	Ok(out)
}

/// An object's decoded properties and the Prior Object ID Gap they mark, if any.
pub(crate) struct Decoded {
	pub properties: Properties,
	pub gap: Option<u64>,
}

/// Decode an object's record body, whose Timestamps are in `timescale`, the units the
/// track declares.
///
/// Only the canonical form is accepted, so what decodes re-encodes to the same bytes.
pub(crate) fn decode_properties(
	body: &[u8],
	timescale: Option<Timescale>,
	version: Version,
) -> Result<Decoded, DecodeError> {
	let mut properties = Properties::default();
	if body.is_empty() {
		return Ok(Decoded { properties, gap: None });
	}
	let r = &mut Decoder::new(body, version.into());
	let flags = r.varint()?;
	let encoding = decode_encoding(flags >> 2)?;
	if flags & TIMESTAMPED != 0 {
		properties.timestamp = Some(r.varint()?);
	}
	let bytes = &body[body.len() - r.remaining()..];
	let mut gap = None;
	match encoding {
		Some(encoding) => {
			// A small cached block must not pin the reader's larger receive buffer.
			let bytes = Bytes::copy_from_slice(bytes);
			let checked = crate::ietf::object::check(&bytes, encoding).map_err(|_| DecodeError::InvalidValue)?;
			properties.sealed_time = checked.sealed_time;
			gap = checked.gap;
			properties.block = Some(crate::model::object::Block {
				bytes,
				encoding,
				immutable: checked.immutable,
				delivery_timeout: flags & DELIVERY_TIMEOUT != 0,
			});
		}
		// Nothing but a Timestamp: DELIVERY_TIMEOUT describes a block, and an empty body
		// is how nothing at all is sent.
		None if !bytes.is_empty() || flags != TIMESTAMPED => return Err(DecodeError::InvalidValue),
		None => {}
	}
	stamp(&mut properties, timescale);
	Ok(Decoded { properties, gap })
}

/// Record that `properties` carry their Timestamps in `timescale`, the units the track
/// declares, when they carry any.
pub(crate) fn stamp(properties: &mut Properties, timescale: Option<Timescale>) {
	properties.timescale = timescale.filter(|_| properties.timestamp.is_some() || properties.sealed_time);
}

/// A group stream's object records on egress, sent before each frame once the extension
/// is negotiated.
#[cfg(feature = "async")]
pub(crate) struct Records {
	/// The previous record's Object ID, `None` before the first.
	prior: Option<u64>,
	/// The units the track declares, which mutable Timestamps convert to.
	timescale: Option<Timescale>,
	version: Version,
}

#[cfg(feature = "async")]
impl Records {
	/// Buffer the Group State for a stream serving `group` from its current index, and
	/// return the records its frames need.
	///
	/// `capped` when the stream stops before the group may end. Writes nothing and returns
	/// `None` unless the session negotiated the extension.
	pub fn start<W: crate::transport::poll::SendStream>(
		writer: &mut crate::coding::Writer<W, Version>,
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
		Ok(Some(Self {
			prior: None,
			timescale,
			version,
		}))
	}

	/// Buffer the record for the frame at `index`, ahead of its timestamp and size.
	pub fn frame<W: crate::transport::poll::SendStream>(
		&mut self,
		writer: &mut crate::coding::Writer<W, Version>,
		index: u64,
		fields: Option<&Fields>,
	) -> Result<(), Error> {
		self.record(writer, fields.map_or(index, |fields| fields.id), fields, false)
	}

	/// Buffer the END_OF_GROUP record once every frame is written, if the group has one.
	pub fn end<W: crate::transport::poll::SendStream>(
		&mut self,
		writer: &mut crate::coding::Writer<W, Version>,
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
		writer: &mut crate::coding::Writer<W, Version>,
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
		// Flags only ever describe values, so none at all is a lone 0.
		let Some(properties) = self.0.as_ref().filter(|properties| !properties.values.is_empty()) else {
			return w.varint(0);
		};
		w.varint(encode_flags(properties.delivery_timeout, properties.immutable))?;
		encode_list(w, &properties.values)
	}

	fn decode_msg(r: &mut Decoder<'_>, _: Version) -> Result<Self, DecodeError> {
		let flags = r.varint()?;
		let (delivery_timeout, encoding) = decode_flags(flags)?;
		let values = decode_list(r)?;
		// TRACK_INFO carries these, so a second copy could only contradict it.
		if values
			.iter()
			.any(|property| crate::ietf::properties::understood(property.id))
		{
			return Err(DecodeError::InvalidValue);
		}
		// The sealed keys are the block's own, so they are parsed rather than trusted.
		let sealed = match (immutable(&values, encoding)?, encoding) {
			(Some(block), Some(encoding)) => crate::ietf::properties::sealed_ids(block, encoding)?,
			_ => Vec::new(),
		};
		if values.is_empty() {
			// Only the canonical form, so what decodes re-encodes to the same bytes.
			return match flags {
				0 => Ok(Self(None)),
				_ => Err(DecodeError::InvalidValue),
			};
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
	w.bytes(&encode_properties(
		&wire.properties,
		wire.properties.timestamp,
		version,
	)?)
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
		for (prior, id, end) in [
			(None, 0, false),
			(None, 7, true),
			(Some(3), 4, false),
			(Some(3), 9, true),
		] {
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

		let partial = decode_state(encode_state(Some(&wire), false, true), 0)
			.unwrap()
			.unwrap();
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
		let values = [
			Property {
				id: 0x06,
				value: Value::Int(5),
			},
			Property {
				id: IMMUTABLE,
				value: Value::Bytes(sealed.into()),
			},
		];
		let properties = Properties {
			timestamp: Some(1_000),
			timescale: Some(Timescale::MILLI),
			sealed_time: true,
			block: crate::ietf::object::block(&values, Encoding::DeltaQuic, true).unwrap(),
		};
		let body = encode_properties(&properties, properties.timestamp, V).unwrap();
		let decoded = decode_properties(&body, Some(Timescale::MILLI), V).unwrap();
		assert_eq!(decoded.properties, properties);
		assert_eq!(decoded.gap, Some(2));

		// None at all is an empty body, and a Timestamp alone needs no block.
		assert!(encode_properties(&Properties::default(), None, V).unwrap().is_empty());
		let empty = decode_properties(&[], Some(Timescale::MILLI), V).unwrap();
		assert_eq!(empty.properties, Properties::default());
		let timed = decode_properties(&[TIMESTAMPED as u8, 7], None, V).unwrap();
		assert_eq!(timed.properties.timestamp, Some(7));
		assert_eq!(
			encode_properties(&timed.properties, Some(7), V).unwrap(),
			[TIMESTAMPED as u8, 7]
		);
	}

	/// Only the canonical form decodes, so whatever decodes re-encodes to the same bytes.
	#[test]
	fn properties_refuse_what_is_not_canonical() {
		let delta = 4 << 2;
		for (body, reason) in [
			(&[0][..], "flags saying nothing"),
			(&[DELIVERY_TIMEOUT as u8][..], "DELIVERY_TIMEOUT without a block"),
			(&[delta][..], "an encoding with an empty block"),
			(&[5 << 2, 0x20, 1][..], "an unknown encoding"),
			(&[delta, 0x10, 5][..], "a mutable Timestamp inside the block"),
			(&[0, 0x20, 1][..], "a block without an encoding"),
			(&[delta, 0x0b, 2, 0x0b, 0][..], "a nested Immutable Properties"),
		] {
			assert!(decode_properties(body, None, V).is_err(), "accepted {reason}");
		}
	}

	/// The worst case for re-encoding, a full moq-transport list of zero-delta properties
	/// after a large first ID, crosses the hop byte for byte and is held at its wire size.
	#[test]
	fn a_full_block_crosses_at_its_wire_size() {
		for version in [crate::ietf::Version::Draft16, crate::ietf::Version::Draft22] {
			let mut list = Vec::new();
			let e = &mut Encoder::new(&mut list, version.into());
			// A Timestamp, then IDs from 2^40 on, each repeated as a two-byte zero delta.
			for value in [0x10, 77, (1 << 40) - 0x10, 1] {
				e.varint(value).unwrap();
			}
			while list.len() + 2 <= crate::ietf::object::MAX_BLOCK {
				list.extend([0, 1]);
			}
			let list = Bytes::from(list);
			let decoded = crate::ietf::object::read(&list, version, Some(Timescale::MILLI)).unwrap();
			let properties = decoded.properties;
			let block = properties.block.as_ref().unwrap();
			assert!(block.bytes.len() < list.len(), "{version}");
			assert_eq!(properties.heap(), block.bytes.len() as u64, "{version}: decoded state");

			let body = encode_properties(&properties, properties.timestamp, V).unwrap();
			assert!(body.len() <= MAX_PROPERTIES, "{version}: {} bytes", body.len());
			let received = decode_properties(&body, Some(Timescale::MILLI), V).unwrap();
			assert_eq!(received.properties, properties, "{version}");
			let written = crate::ietf::object::write(&received.properties, version, Some(Timescale::MILLI)).unwrap();
			assert_eq!(written, list, "{version}: the hop changed the list");
		}
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
		let decoded =
			<TrackProperties as crate::coding::Decode<Version>>::decode(&mut Decoder::new(&buf, V.into()), V).unwrap();
		assert_eq!(decoded.0.as_deref(), Some(&properties));

		let none = crate::coding::Encode::encode_bytes(&TrackProperties(None), V).unwrap();
		let decoded =
			<TrackProperties as crate::coding::Decode<Version>>::decode(&mut Decoder::new(&none, V.into()), V).unwrap();
		assert!(decoded.0.is_none());
	}

	/// A peer cannot make a relay hold or forward more than moq-transport ingress would,
	/// restate what TRACK_INFO carries, or claim sealed keys its block does not hold.
	#[test]
	fn track_properties_refuse_what_ingress_would_not_keep() {
		let decode = |values: Vec<Property>, immutable: Option<Encoding>| {
			let msg = TrackProperties(Some(Arc::new(crate::model::object::TrackProperties {
				values,
				immutable,
				..Default::default()
			})));
			let buf = crate::coding::Encode::encode_bytes(&msg, V).unwrap();
			<TrackProperties as crate::coding::Decode<Version>>::decode(&mut Decoder::new(&buf, V.into()), V)
		};
		let opaque = |id| Property {
			id,
			value: Value::Int(1),
		};

		let max = crate::ietf::properties::MAX_PROPERTIES;
		assert!(decode((0..max).map(|i| opaque(0x40 + 2 * i)).collect(), None).is_ok());
		assert!(
			decode((0..=max).map(|i| opaque(0x40 + 2 * i)).collect(), None).is_err(),
			"more than ingress keeps"
		);
		// TIMESCALE belongs to TRACK_INFO.
		assert!(decode(vec![opaque(0x08)], None).is_err(), "an understood property");

		// The sealed keys come from the block, never the sender.
		let sealed = Property {
			id: IMMUTABLE,
			value: Value::Bytes(Bytes::from_static(&[0x20, 0x01, 0x04, 0x05])),
		};
		let decoded = decode(vec![sealed.clone()], Some(Encoding::Delta { seven: true }))
			.unwrap()
			.0
			.unwrap();
		assert_eq!(decoded.sealed, [0x20, 0x24]);
		assert!(
			decode(vec![sealed], Some(Encoding::Absolute)).is_err(),
			"track properties never use absolute keys"
		);
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

	/// A retained datagram holds its properties as the bytes they arrived as, not a decoded
	/// list many times their size.
	#[test]
	fn datagram_properties_are_held_at_their_wire_size() {
		const COUNT: usize = 500;
		let mut block = vec![0x20, 1];
		for _ in 1..COUNT {
			block.extend([0, 1]);
		}
		let mut body = vec![4 << 2];
		body.extend(&block);
		let mut buf = Vec::new();
		let w = &mut Encoder::new(&mut buf, V.into());
		w.varint(RELAYED).unwrap();
		w.bytes(&body).unwrap();
		let wire = decode_datagram(&mut Decoder::new(&buf, V.into()), V).unwrap().unwrap();
		assert_eq!(wire.properties.heap(), block.len() as u64);
		assert!(wire.properties.heap() < (COUNT * size_of::<Property>()) as u64 / 10);
	}
}

/// Peer a relay behind `upstream` over moq-lite 07, the way a cluster does. The client
/// (downstream) and server (upstream) each offer the `ietf` extension per `offer`.
/// Returns the downstream origin once it learned of `broadcast`, and the sessions to hold.
#[cfg(all(test, feature = "async"))]
pub(crate) async fn test_hop(
	upstream: &crate::origin::Producer,
	broadcast: &str,
	offer: [bool; 2],
) -> (crate::origin::Producer, [crate::Session; 2]) {
	use crate::model::ProduceTest;
	let version = crate::Version::Lite(Version::Lite07);
	let downstream = crate::origin::Config::new(crate::Hop::new(3).unwrap()).produce();
	let (client, server) = crate::transport::memory::pair(crate::transport::memory::Config {
		protocol: Some(version.alpn().to_string()),
		..Default::default()
	});
	let extensions = |ietf| crate::setup::Extensions {
		ietf,
		..Default::default()
	};
	let dial = crate::Client::new()
		.with_versions(version.into())
		.with_extensions(extensions(offer[0]))
		.with_subscriber(downstream.clone());
	let serve = crate::Server::new()
		.with_versions(version.into())
		.with_extensions(extensions(offer[1]))
		.with_publisher(upstream.consume());
	let (client, server) = futures::join!(
		async {
			let (session, driver) = dial.connect(moq_clock::now(), client).await.unwrap();
			drop(moq_clock::sim::spawn(crate::time::run(driver)));
			session
		},
		async {
			let (session, driver) = serve.accept(moq_clock::now(), server).await.unwrap();
			drop(moq_clock::sim::spawn(crate::time::run(driver)));
			session
		},
	);
	downstream
		.consume()
		.routed(broadcast)
		.await
		.expect("the hop announced the broadcast");
	(downstream, [client, server])
}

#[cfg(all(test, feature = "async"))]
mod hop_tests {
	use super::*;
	use crate::{Timestamp, frame, model::ProduceTest, track};
	use std::time::Duration;

	const TIMEOUT: Duration = Duration::from_secs(5);
	/// Prior Object ID Gap.
	const GAP: u64 = 0x3e;

	fn ts(millis: u64) -> Timestamp {
		Timestamp::new(millis, Timescale::MILLI).unwrap()
	}

	fn property(id: u64, value: u64) -> Property {
		Property {
			id,
			value: Value::Int(value),
		}
	}

	/// The block a draft-22 publisher's `values` arrive as.
	fn block(values: &[Property]) -> Option<crate::model::object::Block> {
		crate::ietf::object::block(values, Encoding::Delta { seven: true }, true).unwrap()
	}

	/// Write a frame carrying relay `fields`, as moq-transport ingress does.
	fn write(group: &mut group::Producer, millis: u64, fields: Option<crate::model::object::Fields>, payload: &[u8]) {
		let mut frame = group
			.create_frame_owned(
				frame::Header {
					info: frame::Info {
						size: payload.len() as u64,
						timestamp: Some(ts(millis)),
					},
					fields: fields.map(Arc::new),
				},
				&frame::Budget::default(),
			)
			.unwrap();
		frame.write(Bytes::copy_from_slice(payload)).unwrap();
		frame.finish().unwrap();
	}

	/// A frame's Object ID and properties, when it keeps any.
	fn fields(frame: &frame::Object) -> Option<(u64, Properties)> {
		frame
			.fields
			.as_ref()
			.map(|fields| (fields.id, fields.properties.clone()))
	}

	async fn next(group: &mut group::Consumer) -> Option<frame::Object> {
		moq_clock::timeout(TIMEOUT, kio::wait(|waiter| group.poll_read_object(waiter)))
			.await
			.expect("no frame")
			.unwrap()
	}

	async fn subscribe(origin: &crate::origin::Producer) -> (track::Consumer, track::Subscriber) {
		let broadcast = moq_clock::timeout(TIMEOUT, origin.consume().request_broadcast("room", None))
			.await
			.expect("no broadcast")
			.unwrap();
		let track = broadcast.track("video").unwrap();
		let subscriber = moq_clock::timeout(TIMEOUT, track.subscribe(None))
			.await
			.expect("no subscription")
			.unwrap();
		(track, subscriber)
	}

	async fn recv(subscriber: &mut track::Subscriber) -> group::Consumer {
		moq_clock::timeout(TIMEOUT, subscriber.recv_group())
			.await
			.expect("no group")
			.unwrap()
			.expect("track finished")
	}

	/// A relayed group crosses the hop frame by frame while it is still open, with its
	/// Object IDs, properties, and END_OF_GROUP position, and its track's properties.
	/// A side that does not offer the extension gets the plain frames alone.
	#[moq_clock::test]
	async fn relayed_state_crosses_while_the_group_is_open() {
		for offer in [[true, true], [true, false], [false, true]] {
			let negotiated = offer == [true, true];
			let upstream = crate::origin::Config::new(crate::Hop::new(2).unwrap()).produce();
			let broadcast = upstream.publish("room", crate::origin::Route::default()).unwrap();
			let mut info = track::Info::timed().with_timescale(Timescale::MILLI);
			let track_properties = crate::model::object::TrackProperties {
				values: vec![property(0x40, 3)],
				..Default::default()
			};
			info.properties = Some(Arc::new(track_properties.clone()));
			let track = broadcast.create_track("video", info).unwrap();

			let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
			group
				.set_wire(group::Wire {
					sparse: false,
					first_object: true,
					first_id: 6,
					extensions: true,
					has_end: true,
					fetchable: true,
					last: None,
				})
				.unwrap();
			let first = crate::model::object::Fields {
				id: 6,
				properties: Properties {
					timestamp: Some(0),
					block: block(&[property(0x20, 1), property(GAP, 6)]),
					timescale: Some(Timescale::MILLI),
					..Default::default()
				},
			};
			group.mark_gap(6, Some(6)).unwrap();
			write(&mut group, 0, Some(first.clone()), b"a");

			let (downstream, _sessions) = test_hop(&upstream, "room", offer).await;
			let (copy, mut subscriber) = subscribe(&downstream).await;
			let mut received = recv(&mut subscriber).await;

			// The upstream group is still open.
			let frame = next(&mut received).await.unwrap();
			assert_eq!(&frame.payload[..], b"a");
			assert_eq!(frame.timestamp, Some(ts(0)));
			match negotiated {
				true => {
					assert_eq!(fields(&frame), Some((first.id, first.properties.clone())), "{offer:?}");
					let wire = received.wire().expect("relay state");
					assert!(wire.first_object && wire.extensions && wire.has_end);
					assert_eq!(wire.first_id, 6);
				}
				false => {
					assert!(frame.fields.is_none(), "{offer:?}");
					assert!(received.wire().is_none(), "{offer:?}");
				}
			}

			// Object 9 marks the two it skipped, then the group ends at Object 10.
			let second = crate::model::object::Fields {
				id: 9,
				properties: Properties {
					timestamp: Some(1),
					block: block(&[property(GAP, 2)]),
					timescale: Some(Timescale::MILLI),
					..Default::default()
				},
			};
			group.mark_gap(9, Some(2)).unwrap();
			write(&mut group, 1, Some(second.clone()), b"b");
			group.mark_gap(10, None).unwrap();
			group
				.set_end_object(crate::model::object::Fields {
					id: 10,
					properties: Properties::default(),
				})
				.unwrap();
			group.finish().unwrap();

			let frame = next(&mut received).await.unwrap();
			assert_eq!(&frame.payload[..], b"b");
			assert!(next(&mut received).await.is_none());
			let info = moq_clock::timeout(TIMEOUT, copy.query()).await.unwrap().unwrap();
			match negotiated {
				true => {
					assert_eq!(fields(&frame), Some((second.id, second.properties.clone())));
					assert_eq!(received.end_object().map(|end| end.id), Some(10));
					assert!(received.wire().unwrap().fetchable, "every jump was marked");
					assert_eq!(info.properties.as_deref(), Some(&track_properties));
				}
				false => {
					assert!(frame.fields.is_none());
					assert!(received.end_object().is_none());
					assert!(info.properties.is_none());
				}
			}
		}
	}

	/// Groups and datagrams from a moq-lite publisher cross a negotiated hop as plain as
	/// they left: no relay state appears on the way.
	#[moq_clock::test]
	async fn plain_media_crosses_unchanged() {
		let upstream = crate::origin::Config::new(crate::Hop::new(2).unwrap()).produce();
		let broadcast = upstream.publish("room", crate::origin::Route::default()).unwrap();
		let mut track = broadcast
			.create_track("video", track::Info::timed().with_timescale(Timescale::MILLI))
			.unwrap();
		let (downstream, sessions) = test_hop(&upstream, "room", [true, true]).await;
		let (_copy, mut subscriber) = subscribe(&downstream).await;

		let mut group = track.append_group().unwrap();
		group.write_frame(ts(5), b"key".to_vec()).unwrap();
		let mut received = recv(&mut subscriber).await;
		let frame = next(&mut received).await.unwrap();
		assert_eq!((frame.timestamp, &frame.payload[..]), (Some(ts(5)), &b"key"[..]));
		assert!(frame.fields.is_none() && received.wire().is_none());
		group.write_frame(ts(6), b"delta".to_vec()).unwrap();
		group.finish().unwrap();
		let frame = next(&mut received).await.unwrap();
		assert_eq!((frame.timestamp, &frame.payload[..]), (Some(ts(6)), &b"delta"[..]));
		assert!(frame.fields.is_none());
		assert!(next(&mut received).await.is_none());
		assert!(received.end_object().is_none());

		track.insert_datagram(7, ts(9), b"datagram".to_vec()).unwrap();
		let datagram = moq_clock::timeout(TIMEOUT, kio::wait(|waiter| subscriber.poll_recv_wire_datagram(waiter)))
			.await
			.expect("no datagram")
			.unwrap()
			.unwrap();
		assert_eq!((datagram.sequence, datagram.timestamp), (7, Some(ts(9))));
		assert_eq!(&datagram.payload[..], b"datagram");
		assert!(datagram.wire.is_none());
		drop(sessions);
	}
}
