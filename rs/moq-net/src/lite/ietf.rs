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
	model::object::{Fields, Properties, Property, Value},
};

use super::{Message, Version};

/// The largest record body a peer may make us buffer: the largest block moq-transport
/// admits.
pub(crate) const MAX_PROPERTIES: usize = crate::ietf::object::MAX_BLOCK;

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

/// Track Properties delta-encoded as the publisher's draft encodes them, as moq-transport
/// sent them less the properties TRACK_INFO carries, so the list is never wider than it
/// arrived.
///
/// An even ID has an integer value, an odd one bytes.
fn encode_list(properties: &crate::model::object::TrackProperties) -> Result<Vec<u8>, EncodeError> {
	let mut out = Vec::new();
	let w = &mut Encoder::new(&mut out, properties.version.into());
	let mut prior = 0;
	for property in &properties.values {
		w.varint(property.id.checked_sub(prior).ok_or(EncodeError::InvalidState)?)?;
		prior = property.id;
		match (&property.value, property.id % 2) {
			(Value::Int(value), 0) => w.varint(*value)?,
			(Value::Bytes(value), 1) => w.bytes(value)?,
			_ => return Err(EncodeError::InvalidState),
		}
	}
	Ok(out)
}

/// Whether TRACK_PROPERTIES can carry `properties` across a hop.
///
/// Dropping the understood properties can widen the delta that spans them, and the list
/// shares the message with its Version, so a list that fit its moq-transport message may
/// still not fit, though only by a few bytes.
pub(crate) fn fits(properties: &crate::model::object::TrackProperties) -> bool {
	let code = crate::Version::Ietf(properties.version).code();
	let Ok(code) = crate::coding::varint::size(code, Version::Lite07.into()) else {
		return false;
	};
	encode_list(properties).is_ok_and(|list| code + list.len() <= super::message::MAX_MESSAGE_SIZE)
}

/// Decode Track Properties to the end of `r`, delta-encoded as `version` encodes them.
///
/// No more than moq-transport ingress keeps, which is all a relay forwards.
fn decode_list(r: &mut Decoder<'_>, version: crate::ietf::Version) -> Result<Vec<Property>, DecodeError> {
	let r = &mut Decoder::new(r.rest(), version.into());
	let mut values: Vec<Property> = Vec::new();
	let mut prior = 0u64;
	while !r.is_empty() {
		if values.len() as u64 >= crate::ietf::properties::MAX_PROPERTIES {
			return Err(DecodeError::TooMany);
		}
		let id = prior.checked_add(r.varint()?).ok_or(DecodeError::BoundsExceeded)?;
		prior = id;
		let value = match id % 2 {
			0 => Value::Int(r.varint()?),
			// A small cached property must not pin the reader's larger receive buffer.
			_ => Value::Bytes(Bytes::copy_from_slice(r.bytes()?)),
		};
		values.push(Property { id, value });
	}
	Ok(values)
}

/// An object's properties as a record body, empty when it has none.
///
/// The block is sent as it is held. A timed track's mutable Timestamp is the frame's, so
/// it is never sent here.
fn encode_properties(properties: &Properties) -> &[u8] {
	properties.block.as_ref().map_or(&[], |block| &block.bytes)
}

/// An object's decoded properties and the Prior Object ID Gap they mark, if any.
pub(crate) struct Decoded {
	pub properties: Properties,
	pub gap: Option<u64>,
}

/// Decode an object's record body on a track `framing` describes: the units its sealed
/// Timestamp is in, and the draft that serialized its block. A `framed` object's frame
/// holds its time; a status object's block holds it.
///
/// Only the canonical form is accepted, so what decodes re-encodes to the same bytes.
pub(crate) fn decode_properties(body: &[u8], framing: super::Framing, framed: bool) -> Result<Decoded, DecodeError> {
	if body.is_empty() {
		return Ok(Decoded {
			properties: Properties::default(),
			gap: None,
		});
	}
	// A track published over moq-lite has no moq-transport properties.
	let version = framing.ietf.ok_or(DecodeError::InvalidValue)?;
	// A small cached block must not pin the reader's larger receive buffer.
	let bytes = Bytes::copy_from_slice(body);
	let checked = crate::ietf::object::check(&bytes, version, framing.timescale, framed)
		.map_err(|_| DecodeError::InvalidValue)?;
	Ok(Decoded {
		properties: Properties {
			sealed_time: checked.sealed_time,
			block: Some(crate::model::object::Block {
				bytes,
				immutable: checked.immutable,
			}),
		},
		gap: checked.gap,
	})
}

/// Decode the END_OF_GROUP record's body on a track `framing` describes. It is a status
/// object, so it may not carry properties.
#[cfg(feature = "async")]
pub(crate) fn decode_end(body: &[u8], framing: super::Framing) -> Result<Decoded, DecodeError> {
	let decoded = decode_properties(body, framing, false)?;
	if decoded.properties.block.is_some() {
		return Err(DecodeError::InvalidValue);
	}
	Ok(decoded)
}

/// A group stream's object records on egress, sent before each frame once the extension
/// is negotiated.
#[cfg(feature = "async")]
pub(crate) struct Records {
	/// The previous record's Object ID, `None` before the first.
	prior: Option<u64>,
	/// The stream stops at a frame cap, so its Group State promised no END_OF_GROUP.
	capped: bool,
	/// The units the track declares, which a sealed Timestamp must already be in.
	timescale: Option<Timescale>,
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
	) -> Result<Option<Self>, Error> {
		if !ietf {
			return Ok(None);
		}
		let wire = group.wire();
		let whole = group.index() <= group.first_frame();
		writer.buffer_varint(encode_state(wire.as_ref(), whole, capped))?;
		Ok(Some(Self {
			prior: None,
			capped,
			timescale,
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
		// A capped stream stops at its cap even when that is the last frame, and one capped
		// short of the last frame stops before the end.
		if self.capped || group.index() < group.frame_count() as u64 {
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
				// A sealed Timestamp keeps its units, which the receiver reads as the track's.
				if fields
					.properties
					.sealed_time
					.is_some_and(|sealed| self.timescale != Some(sealed.scale()))
				{
					return Err(Error::Unsupported);
				}
				encode_properties(&fields.properties)
			}
			None => &[],
		};
		writer.buffer_varint(block.len() as u64)?;
		writer.buffer_raw(block);
		Ok(())
	}
}

/// The moq-transport Track Properties a relay forwards, sent on the Track Stream right
/// after TRACK_INFO when the extension is negotiated.
#[derive(Clone, Debug, Default)]
pub struct TrackProperties(pub Option<Arc<crate::model::object::TrackProperties>>);

impl<'a> Message<'a> for TrackProperties {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::TrackInfo);
	fn encode_msg(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		// A track published over moq-lite is a lone 0.
		let Some(properties) = &self.0 else {
			return w.varint(0);
		};
		w.varint(crate::Version::Ietf(properties.version).code())?;
		w.slice(&encode_list(properties)?);
		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'a>, _: Version) -> Result<Self, DecodeError> {
		let code = r.varint()?;
		if code == 0 {
			// A track published over moq-lite has no moq-transport properties.
			return match r.is_empty() {
				true => Ok(Self(None)),
				false => Err(DecodeError::InvalidValue),
			};
		}
		let Some(crate::Version::Ietf(version)) = crate::Version::from_code(code) else {
			return Err(DecodeError::InvalidValue);
		};
		let properties = crate::model::object::TrackProperties {
			version,
			values: decode_list(r, version)?,
		};
		// TRACK_INFO carries these, so a second copy could only contradict it.
		if properties
			.values
			.iter()
			.any(|property| crate::ietf::properties::understood(property.id))
		{
			return Err(DecodeError::InvalidValue);
		}
		// Egress reads the sealed keys from the block, so it must be the one ingress would keep.
		if properties
			.values
			.iter()
			.filter(|property| property.id == IMMUTABLE)
			.count() > 1
		{
			return Err(DecodeError::InvalidValue);
		}
		if let Some(block) = properties.immutable() {
			crate::ietf::properties::check_immutable(block, version)?;
		}
		Ok(Self(Some(Arc::new(properties))))
	}
}

/// Encode a datagram's relay state after its Group Sequence, or a plain one's as 0.
pub(crate) fn encode_datagram(
	w: &mut Encoder<'_>,
	wire: Option<&crate::model::datagram::Wire>,
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
	w.bytes(encode_properties(&wire.properties))
}

/// Decode a datagram's relay state on a track `framing` describes.
pub(crate) fn decode_datagram(
	r: &mut Decoder<'_>,
	framing: super::Framing,
) -> Result<Option<crate::model::datagram::Wire>, DecodeError> {
	let state = r.varint()?;
	if state == 0 {
		return Ok(None);
	}
	if state & RELAYED == 0 || state & !(RELAYED | OBJECT_ID | PRIORITY | DATAGRAM_END | STATUS) != 0 {
		return Err(DecodeError::InvalidValue);
	}
	// A track published over moq-lite has no moq-transport state.
	framing.ietf.ok_or(DecodeError::InvalidValue)?;
	let object_id = (state & OBJECT_ID != 0).then(|| r.varint()).transpose()?;
	let priority = (state & PRIORITY != 0).then(|| r.u8()).transpose()?;
	let status = (state & STATUS != 0).then(|| r.varint()).transpose()?;
	// A status object has no frame to hold its time.
	let framed = status.is_none_or(|status| status == 0);
	let block = r.bytes()?;
	let properties = decode_properties(block, framing, framed)?.properties;
	let wire = crate::model::datagram::Wire {
		object_id,
		priority,
		end_of_group: state & DATAGRAM_END != 0,
		status,
		properties,
	};
	// A relay holds and forwards it, so it must be one its publisher's draft could send.
	if !wire.defined() {
		return Err(DecodeError::InvalidValue);
	}
	Ok(Some(wire))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ietf::Version as Draft;

	const V: Version = Version::Lite07;

	/// A track in `timescale` that a `version` moq-transport publisher published.
	fn from(version: Draft, timescale: Option<Timescale>) -> super::super::Framing {
		super::super::Framing {
			timescale,
			ietf: Some(version),
			compressed: false,
		}
	}

	/// The moq-transport version code TRACK_PROPERTIES carries for `version`.
	fn code(version: Draft) -> u64 {
		crate::Version::Ietf(version).code()
	}

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
	fn properties_round_trip() {
		// An immutable block holding a gap of 2 (0x3e) and a timestamp.
		let mut sealed = Vec::new();
		let e = &mut Encoder::new(&mut sealed, Draft::Draft18.into());
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
			sealed_time: Some(crate::Timestamp::new(77, Timescale::MILLI).unwrap()),
			block: crate::ietf::object::block(&values, Draft::Draft18).unwrap(),
		};
		let body = encode_properties(&properties);
		let decoded = decode_properties(body, from(Draft::Draft18, Some(Timescale::MILLI)), true).unwrap();
		assert_eq!(decoded.properties, properties);
		assert_eq!(decoded.gap, Some(2));

		// None at all is an empty body.
		assert!(encode_properties(&Properties::default()).is_empty());
		let empty = decode_properties(&[], from(Draft::Draft18, Some(Timescale::MILLI)), true).unwrap();
		assert_eq!(empty.properties, Properties::default());

		// An untimed track's Timestamp has no frame to hold it, so it stays in the block.
		let untimed = [0x10, 7];
		let decoded = decode_properties(&untimed, from(Draft::Draft18, None), true).unwrap();
		assert_eq!(encode_properties(&decoded.properties), untimed);
	}

	/// Only the canonical form decodes, so whatever decodes re-encodes to the same bytes.
	#[test]
	fn properties_refuse_what_is_not_canonical() {
		let timed = from(Draft::Draft18, Some(Timescale::MILLI));
		for (body, reason) in [
			(
				&[0x10, 5][..],
				"a timed track's mutable Timestamp, which its frame holds",
			),
			(&[0x0b, 2, 0x0b, 0][..], "a nested Immutable Properties"),
		] {
			assert!(decode_properties(body, timed, true).is_err(), "accepted {reason}");
		}
		// A track published over moq-lite has no moq-transport properties.
		let lite = super::super::Framing { ietf: None, ..timed };
		assert!(decode_properties(&[0x20, 1], lite, true).is_err());
	}

	/// The worst case for re-encoding, a full moq-transport list of zero-delta properties
	/// after a large first ID, crosses the hop byte for byte and is held at its wire size.
	#[test]
	fn a_full_block_crosses_at_its_wire_size() {
		for version in [Draft::Draft22] {
			let mut list = Vec::new();
			let e = &mut Encoder::new(&mut list, version.into());
			// A Timestamp, then IDs from 2^40 on, each repeated as a two-byte zero delta.
			for value in [0x10, 77, (1 << 40) - 0x10, 1] {
				e.varint(value).unwrap();
			}
			// The largest a timed track's block may be, once its Timestamp leaves it.
			while list.len() <= crate::ietf::object::max_block(Some(Timescale::MILLI)) {
				list.extend([0, 1]);
			}
			let list = Bytes::from(list);
			let decoded = crate::ietf::object::read(&list, version, Some(Timescale::MILLI)).unwrap();
			let frame = decoded.timestamp;
			let properties = decoded.properties;
			let block = properties.block.as_ref().unwrap();
			assert!(block.bytes.len() < list.len(), "{version}");
			assert_eq!(properties.heap(), block.bytes.len() as u64, "{version}");

			let body = encode_properties(&properties);
			assert!(body.len() <= MAX_PROPERTIES, "{version}: {} bytes", body.len());
			let received = decode_properties(body, from(version, Some(Timescale::MILLI)), true).unwrap();
			assert_eq!(received.properties, properties, "{version}");
			let written = crate::ietf::object::write(
				&received.properties,
				Some(version),
				frame,
				version,
				Some(Timescale::MILLI),
			)
			.unwrap();
			assert_eq!(written, list, "{version}: the hop changed the list");
		}
	}

	fn round_trip(msg: &TrackProperties) -> Result<TrackProperties, DecodeError> {
		let buf = crate::coding::Encode::encode_bytes(msg, V).unwrap();
		<TrackProperties as crate::coding::Decode<Version>>::decode(&mut Decoder::new(&buf, V.into()), V)
	}

	/// A moq-transport track carries its publisher's draft once, with or without
	/// properties, and a moq-lite track carries none.
	#[test]
	fn track_properties_round_trip() {
		let properties = crate::model::object::TrackProperties {
			version: Draft::Draft18,
			values: vec![
				Property {
					id: IMMUTABLE,
					value: Value::Bytes(Bytes::from_static(&[0x02, 0x01])),
				},
				Property {
					id: 0x41,
					value: Value::Bytes(Bytes::from_static(b"opaque")),
				},
			],
		};
		let decoded = round_trip(&TrackProperties(Some(Arc::new(properties.clone())))).unwrap();
		assert_eq!(decoded.0.as_deref(), Some(&properties));

		// An empty list still names its draft.
		let bare = crate::model::object::TrackProperties {
			version: Draft::Draft22,
			values: Vec::new(),
		};
		let decoded = round_trip(&TrackProperties(Some(Arc::new(bare.clone())))).unwrap();
		assert_eq!(decoded.0.as_deref(), Some(&bare));

		let none = crate::coding::Encode::encode_bytes(&TrackProperties(None), V).unwrap();
		assert_eq!(&none[1..], [0], "a moq-lite track is a lone 0");
		assert!(round_trip(&TrackProperties(None)).unwrap().0.is_none());
	}

	/// A peer cannot make a relay hold or forward more than moq-transport ingress would,
	/// restate what TRACK_INFO carries, claim a draft it could not have come from, or
	/// claim sealed keys its block does not hold.
	#[test]
	fn track_properties_refuse_what_ingress_would_not_keep() {
		let wire = |values: &[Property], code: u64| {
			let mut buf = Vec::new();
			let w = &mut Encoder::new(&mut buf, V.into());
			w.varint(code).unwrap();
			let list = encode_list(&crate::model::object::TrackProperties {
				version: Draft::Draft18,
				values: values.to_vec(),
			})
			.unwrap();
			w.slice(&list);
			buf
		};
		let decode = |buf: &[u8]| TrackProperties::decode_msg(&mut Decoder::new(buf, V.into()), V);
		let opaque = |id| Property {
			id,
			value: Value::Int(1),
		};
		let draft18 = code(Draft::Draft18);

		let max = crate::ietf::properties::MAX_PROPERTIES;
		let list: Vec<_> = (0..=max).map(|i| opaque(0x40 + 2 * i)).collect();
		assert!(decode(&wire(&list[..max as usize], draft18)).is_ok());
		assert!(decode(&wire(&list, draft18)).is_err(), "more than ingress keeps");
		// TIMESCALE belongs to TRACK_INFO.
		assert!(
			decode(&wire(&[opaque(0x08)], draft18)).is_err(),
			"an understood property"
		);
		assert!(
			decode(&wire(&[opaque(0x40)], 0)).is_err(),
			"properties on a moq-lite track"
		);
		for code in [1, 0xff000099, crate::Version::Lite(V).code()] {
			assert!(decode(&wire(&[], code)).is_err(), "{code:#x} is no moq-transport draft");
		}

		// The sealed keys come from the block, never the sender.
		let sealed = Property {
			id: IMMUTABLE,
			value: Value::Bytes(Bytes::from_static(&[0x20, 0x01, 0x04, 0x05])),
		};
		let decoded = decode(&wire(std::slice::from_ref(&sealed), draft18))
			.unwrap()
			.0
			.unwrap();
		assert!(decoded.sealed().eq([0x20, 0x24]));
		assert!(
			decode(&wire(&[sealed.clone(), sealed], draft18)).is_err(),
			"a second block leaves the sealed keys ambiguous"
		);
	}

	/// Sealed keys are read from the Immutable Properties block, never held beside it as a
	/// list that could cost several times the block.
	#[test]
	fn track_properties_hold_sealed_keys_in_their_block() {
		// Nearly as many two-byte keys as a lite message holds: 0x20 = 1, then zero deltas.
		let count = 32_000;
		let mut sealed = vec![0x20, 1];
		sealed.extend([0, 1].repeat(count - 1));
		let msg = TrackProperties(Some(Arc::new(crate::model::object::TrackProperties {
			version: Draft::Draft18,
			values: vec![Property {
				id: IMMUTABLE,
				value: Value::Bytes(sealed.into()),
			}],
		})));
		let decoded = round_trip(&msg).unwrap().0.unwrap();
		assert_eq!(decoded.values.len(), 1);
		assert_eq!(decoded.sealed().count(), count);
		assert!(decoded.sealed().all(|id| id == 0x20));
	}

	/// Track Properties cross in the delta encoding moq-transport sent, where a repeated ID
	/// is a one-byte zero. Written with absolute IDs, a list that filled its moq-transport
	/// message would no longer fit a lite one.
	#[test]
	fn track_properties_cross_at_their_wire_size() {
		for version in [Draft::Draft22] {
			// As many repeats of an ID from 2^60 as moq-transport forwards, then one value
			// that fills the rest of a lite message beside its Version.
			let id = (1 << 60) + 1;
			let repeats = crate::ietf::properties::MAX_FORWARDED - 1;
			let mut list = Vec::new();
			let e = &mut Encoder::new(&mut list, version.into());
			e.varint(id).unwrap();
			e.bytes(&[]).unwrap();
			for _ in 1..repeats {
				e.varint(0).unwrap();
				e.bytes(&[]).unwrap();
			}
			let prefix = crate::coding::varint::size(code(version), V.into()).unwrap();
			let room = super::super::message::MAX_MESSAGE_SIZE - prefix;
			let rest = room - list.len() - 1;
			let fill = (1..=8)
				.map(|prefix| rest - prefix)
				.find(|fill| {
					crate::coding::varint::size(*fill as u64, version.into()).is_ok_and(|size| size == rest - fill)
				})
				.unwrap();
			let e = &mut Encoder::new(&mut list, version.into());
			e.varint(0).unwrap();
			e.bytes(&vec![0; fill]).unwrap();
			assert_eq!(list.len(), room, "{version}");

			let ingress = crate::ietf::Properties::decode(&mut Decoder::new(&list, version.into()), version).unwrap();
			assert_eq!(ingress.unknown.len(), repeats + 1, "{version}");
			let forwarded = crate::model::object::TrackProperties {
				version,
				values: ingress.unknown.clone(),
			};
			assert!(forwarded.refused(&crate::track::Info::default()).is_none(), "{version}");

			let decoded = round_trip(&TrackProperties(Some(Arc::new(forwarded.clone())))).unwrap();
			assert_eq!(decoded.0.as_deref(), Some(&forwarded), "{version}");

			let mut written = Vec::new();
			let egress = crate::ietf::Properties {
				unknown: decoded.0.unwrap().values.clone(),
				..Default::default()
			};
			egress
				.encode(&mut Encoder::new(&mut written, version.into()), version)
				.unwrap();
			assert_eq!(written, list, "{version}: the hop changed the list");

			// A byte more would not fit beside the Version.
			let mut wider = forwarded;
			let Value::Bytes(last) = &mut wider.values.last_mut().unwrap().value else {
				unreachable!()
			};
			*last = vec![0; fill + 1].into();
			assert!(!fits(&wider), "{version}");
		}
	}

	fn datagram(wire: &crate::model::datagram::Wire) -> Vec<u8> {
		let mut buf = Vec::new();
		encode_datagram(&mut Encoder::new(&mut buf, V.into()), Some(wire)).unwrap();
		buf
	}

	/// A relay holds and forwards a datagram's Status, so one moq-transport does not define
	/// would close the session it reached: it is refused where it arrives.
	#[test]
	fn datagram_statuses_are_ones_moq_transport_defines() {
		for (status, ok) in [(0, true), (1, false), (2, false), (3, true), (4, true), (5, false)] {
			let wire = crate::model::datagram::Wire {
				object_id: Some(3),
				priority: Some(9),
				end_of_group: false,
				status: Some(status),
				properties: Properties::default(),
			};
			let decoded = decode_datagram(
				&mut Decoder::new(&datagram(&wire), V.into()),
				from(Draft::Draft18, None),
			);
			assert_eq!(decoded.is_ok(), ok, "{status}");
		}
	}

	/// No moq-transport draft defines a status datagram that also ends the group, so one
	/// is refused where it arrives, by the check moq-transport ingress applies.
	#[test]
	fn a_status_datagram_cannot_end_the_group() {
		let wire = crate::model::datagram::Wire {
			object_id: Some(3),
			priority: Some(9),
			end_of_group: true,
			status: Some(3),
			properties: Properties::default(),
		};
		assert!(
			decode_datagram(
				&mut Decoder::new(&datagram(&wire), V.into()),
				from(Draft::Draft22, None)
			)
			.is_err()
		);
	}

	#[test]
	fn datagram_state_round_trips() {
		for (end_of_group, status) in [(false, Some(3)), (true, None)] {
			let wire = crate::model::datagram::Wire {
				object_id: Some(3),
				priority: Some(9),
				end_of_group,
				status,
				properties: Properties::default(),
			};
			let decoded = decode_datagram(
				&mut Decoder::new(&datagram(&wire), V.into()),
				from(Draft::Draft22, None),
			)
			.unwrap();
			assert_eq!(decoded, Some(wire));
		}

		let mut plain = Vec::new();
		encode_datagram(&mut Encoder::new(&mut plain, V.into()), None).unwrap();
		assert_eq!(plain, [0]);
		// A plain datagram crosses whether or not its track came from moq-transport.
		for framing in [from(Draft::Draft22, None), Default::default()] {
			assert_eq!(
				decode_datagram(&mut Decoder::new(&plain, V.into()), framing).unwrap(),
				None
			);
		}
	}

	/// A track published over moq-lite has no moq-transport state, so a datagram on one
	/// with any is refused.
	#[test]
	fn datagram_state_needs_a_moq_transport_track() {
		let id = crate::model::datagram::Wire {
			object_id: Some(3),
			priority: None,
			end_of_group: false,
			status: None,
			properties: Properties::default(),
		};
		let properties = crate::model::datagram::Wire {
			object_id: None,
			properties: Properties {
				block: crate::ietf::object::block(
					&[Property {
						id: 0x20,
						value: Value::Int(1),
					}],
					Draft::Draft22,
				)
				.unwrap(),
				..Default::default()
			},
			..id.clone()
		};
		for wire in [id, properties] {
			let buf = datagram(&wire);
			assert!(decode_datagram(&mut Decoder::new(&buf, V.into()), from(Draft::Draft22, None)).is_ok());
			assert!(
				decode_datagram(&mut Decoder::new(&buf, V.into()), Default::default()).is_err(),
				"{wire:?}"
			);
		}
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
		let mut buf = Vec::new();
		let w = &mut Encoder::new(&mut buf, V.into());
		w.varint(RELAYED).unwrap();
		w.bytes(&block).unwrap();
		let wire = decode_datagram(&mut Decoder::new(&buf, V.into()), from(Draft::Draft18, None))
			.unwrap()
			.unwrap();
		assert_eq!(wire.properties.heap(), block.len() as u64);
		assert!(wire.properties.heap() < (COUNT * size_of::<Property>()) as u64 / 10);
	}

	/// Drafts 18 and 22 serialize a block alike, so only the track's draft says whether 0x06
	/// is SUBGROUP_DELIVERY_TIMEOUT: a draft-18 one is dropped on its way to draft-22.
	#[test]
	fn a_block_means_what_its_tracks_draft_says() {
		for (version, kept) in [(Draft::Draft18, false), (Draft::Draft22, true)] {
			let decoded = decode_properties(&[0x06, 42], from(version, None), true).unwrap();
			let written =
				crate::ietf::object::write(&decoded.properties, Some(version), None, Draft::Draft22, None).unwrap();
			assert_eq!(!written.is_empty(), kept, "{version}");
		}
	}

	/// A status object carries no properties, so the END_OF_GROUP record has no block.
	#[cfg(feature = "async")]
	#[test]
	fn an_end_record_has_no_block() {
		let framing = from(Draft::Draft22, None);
		assert!(decode_end(&[0x20, 1], framing).is_err());
		assert!(decode_end(&[], framing).is_ok());
	}

	/// A datagram must be one its track's draft defines: a non-zero Status carries no
	/// properties.
	#[test]
	fn a_datagram_block_needs_a_draft_that_sends_its_shape() {
		for (status, carried) in [(0, true), (1, false), (3, false), (4, false)] {
			for version in [Draft::Draft18, Draft::Draft22] {
				// Object 3 at priority 9, which every draft can send beside a status.
				let mut buf = Vec::new();
				let w = &mut Encoder::new(&mut buf, V.into());
				w.varint(RELAYED | OBJECT_ID | PRIORITY | STATUS).unwrap();
				w.varint(3).unwrap();
				w.u8(9);
				w.varint(status).unwrap();
				w.bytes(&[0x20, 1]).unwrap();
				let decoded = decode_datagram(&mut Decoder::new(&buf, V.into()), from(version, None));
				assert_eq!(decoded.is_ok(), carried, "{status}: {version}");
			}
		}
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
		.routes(broadcast)
		.unwrap()
		.next()
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
		crate::ietf::object::block(values, crate::ietf::Version::Draft22).unwrap()
	}

	/// A draft-22 publisher's track with `values`.
	fn draft22(values: Vec<Property>) -> Option<Arc<crate::model::object::TrackProperties>> {
		Some(Arc::new(crate::model::object::TrackProperties {
			version: crate::ietf::Version::Draft22,
			values,
		}))
	}

	/// Write a frame carrying relay `fields`, as moq-transport ingress does, returning the
	/// group's writer.
	fn write(
		group: group::Producer,
		millis: u64,
		fields: Option<crate::model::object::Fields>,
		payload: &[u8],
	) -> group::Producer {
		let mut frame = group
			.create_object(frame::Header {
				info: frame::Info {
					size: payload.len() as u64,
					timestamp: Some(ts(millis)),
				},
				fields: fields.map(Arc::new),
				inflated: None,
			})
			.unwrap();
		frame.write(Bytes::copy_from_slice(payload)).unwrap();
		frame.finish().unwrap()
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
		let broadcast = moq_clock::timeout(TIMEOUT, origin.consume().request("room", None))
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
	/// A side that does not offer the extension gets the plain frames alone. The track info
	/// moq-transport egress restates as Track Properties crosses either way.
	#[moq_clock::test]
	async fn relayed_state_crosses_while_the_group_is_open() {
		for offer in [[true, true], [true, false], [false, true]] {
			let negotiated = offer == [true, true];
			let upstream = crate::origin::Config::new(crate::Hop::new(2).unwrap()).produce();
			let broadcast = upstream.publish("room", crate::origin::Route::default()).unwrap();
			let mut info = track::Info::timed().with_timescale(Timescale::MILLI);
			info.max_age = Some(Duration::from_secs(7));
			info.priority = 9;
			info.refresh = Some(0);
			let track_properties = draft22(vec![property(0x40, 3)]);
			info.properties = track_properties.clone();
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
					block: block(&[property(0x20, 1), property(GAP, 6)]),
					..Default::default()
				},
			};
			group.mark_gap(6, Some(6)).unwrap();
			group = write(group, 0, Some(first.clone()), b"a");

			let (downstream, _sessions) = test_hop(&upstream, "room", offer).await;
			let (copy, mut subscriber) = subscribe(&downstream).await;
			let mut received = recv(&mut subscriber).await;

			// The upstream group is still open.
			let frame = next(&mut received).await.unwrap();
			assert_eq!(&frame.payload.contiguous(0)[..], b"a");
			assert_eq!(frame.timestamp, negotiated.then(|| ts(0)));
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
					block: block(&[property(GAP, 2)]),
					..Default::default()
				},
			};
			group.mark_gap(9, Some(2)).unwrap();
			group = write(group, 1, Some(second.clone()), b"b");
			group.mark_gap(10, None).unwrap();
			group
				.set_end_object(crate::model::object::Fields {
					id: 10,
					properties: Properties::default(),
				})
				.unwrap();
			group.finish().unwrap();

			let frame = next(&mut received).await.unwrap();
			assert_eq!(&frame.payload.contiguous(0)[..], b"b");
			assert!(next(&mut received).await.is_none());
			let info = moq_clock::timeout(TIMEOUT, copy.info()).await.unwrap().unwrap();
			assert_eq!(
				(info.max_age, info.timescale, info.priority, info.refresh),
				(
					Some(Duration::from_secs(7)),
					negotiated.then_some(Timescale::MILLI),
					9,
					Some(0)
				),
				"{offer:?}"
			);
			match negotiated {
				true => {
					assert_eq!(fields(&frame), Some((second.id, second.properties.clone())));
					assert_eq!(received.end_object().map(|end| end.id), Some(10));
					assert!(received.wire().unwrap().fetchable, "every jump was marked");
					assert_eq!(info.properties, track_properties);
				}
				false => {
					assert!(frame.fields.is_none());
					assert!(received.end_object().is_none());
					assert!(info.properties.is_none());
				}
			}
		}
	}

	/// Subscribe across a negotiated hop to a timed track a draft-22 publisher published
	/// with `values`.
	async fn subscribe_across(values: Vec<Property>) -> Result<(), crate::Error> {
		let mut info = track::Info::timed().with_timescale(Timescale::MILLI);
		info.properties = draft22(values);
		subscribe_across_info(info).await
	}

	/// Subscribe across a negotiated hop to a track with `info`.
	async fn subscribe_across_info(info: track::Info) -> Result<(), crate::Error> {
		let upstream = crate::origin::Config::new(crate::Hop::new(2).unwrap()).produce();
		let broadcast = upstream.publish("room", crate::origin::Route::default()).unwrap();
		let _track = broadcast.create_track("video", info).unwrap();

		let (downstream, _sessions) = test_hop(&upstream, "room", [true, true]).await;
		let broadcast = moq_clock::timeout(TIMEOUT, downstream.consume().request("room", None))
			.await
			.expect("no broadcast")
			.unwrap();
		moq_clock::timeout(TIMEOUT, broadcast.track("video").unwrap().subscribe(None))
			.await
			.expect("no answer")
			.map(drop)
	}

	/// A track with a Mandatory Track Property, mutable or sealed, is refused across the hop
	/// as moq-transport ingress refuses it: one we do not understand is neither processed
	/// nor forwarded.
	#[moq_clock::test]
	async fn mandatory_track_properties_are_refused() {
		let mut sealed = Vec::new();
		let e = &mut Encoder::new(&mut sealed, crate::ietf::Version::Draft22.into());
		e.varint(0x4000).unwrap();
		e.varint(1).unwrap();
		for values in [
			vec![property(0x4000, 1)],
			vec![Property {
				id: IMMUTABLE,
				value: Value::Bytes(sealed.into()),
			}],
		] {
			let res = subscribe_across(values.clone()).await;
			assert!(
				matches!(res, Err(crate::Error::UnsupportedExtension)),
				"{values:?}: {res:?}"
			);
		}
	}

	/// A sealed TIMESCALE that contradicts TRACK_INFO is refused across the hop: the relay
	/// times frames by TRACK_INFO, while egress would forward the sealed one.
	#[moq_clock::test]
	async fn a_sealed_property_must_match_track_info() {
		for (timescale, ok) in [(1000, true), (900, false), (0, false)] {
			let mut sealed = Vec::new();
			let e = &mut Encoder::new(&mut sealed, crate::ietf::Version::Draft22.into());
			e.varint(0x08).unwrap();
			e.varint(timescale).unwrap();
			let res = subscribe_across(vec![Property {
				id: IMMUTABLE,
				value: Value::Bytes(sealed.into()),
			}])
			.await;
			match ok {
				true => assert!(res.is_ok(), "{timescale}: {res:?}"),
				false => assert!(matches!(res, Err(crate::Error::Unsupported)), "{timescale}: {res:?}"),
			}
		}
	}

	/// A sealed DYNAMIC_GROUPS must match the refresh TRACK_INFO carries across the hop,
	/// which moq-transport egress restates as DYNAMIC_GROUPS.
	#[moq_clock::test]
	async fn a_sealed_dynamic_groups_must_match_track_info() {
		for (refresh, value, ok) in [
			(Some(0), 1, true),
			(None, 0, true),
			(Some(0), 0, false),
			(None, 1, false),
		] {
			let mut sealed = Vec::new();
			let e = &mut Encoder::new(&mut sealed, crate::ietf::Version::Draft22.into());
			e.varint(0x30).unwrap();
			e.varint(value).unwrap();
			let mut info = track::Info::timed().with_timescale(Timescale::MILLI);
			info.refresh = refresh;
			info.properties = draft22(vec![Property {
				id: IMMUTABLE,
				value: Value::Bytes(sealed.into()),
			}]);
			let res = subscribe_across_info(info).await;
			match ok {
				true => assert!(res.is_ok(), "{refresh:?} {value}: {res:?}"),
				false => assert!(
					matches!(res, Err(crate::Error::Unsupported)),
					"{refresh:?} {value}: {res:?}"
				),
			}
		}
	}

	/// A relay forwards only as many Track Properties as leave room for the ones it
	/// restates from TRACK_INFO, so one more is refused across the hop.
	#[moq_clock::test]
	async fn forwarded_track_properties_leave_room_for_track_info() {
		let max = crate::ietf::properties::MAX_FORWARDED;
		for (count, ok) in [(max, true), (max + 1, false)] {
			let res = subscribe_across((0..count as u64).map(|i| property(0x40 + 2 * i, 1)).collect()).await;
			match ok {
				true => assert!(res.is_ok(), "{count}: {res:?}"),
				false => assert!(matches!(res, Err(crate::Error::Unsupported)), "{count}: {res:?}"),
			}
		}
	}

	/// Groups and datagrams from a moq-lite publisher cross a negotiated hop as plain as
	/// they left: no relay state appears on the way, nor a moq-transport draft.
	#[moq_clock::test]
	async fn plain_media_crosses_unchanged() {
		let upstream = crate::origin::Config::new(crate::Hop::new(2).unwrap()).produce();
		let broadcast = upstream.publish("room", crate::origin::Route::default()).unwrap();
		let mut track = broadcast
			.create_track("video", track::Info::timed().with_timescale(Timescale::MILLI))
			.unwrap();
		let (downstream, sessions) = test_hop(&upstream, "room", [true, true]).await;
		let (copy, mut subscriber) = subscribe(&downstream).await;
		let info = moq_clock::timeout(TIMEOUT, copy.info()).await.unwrap().unwrap();
		assert!(info.properties.is_none());

		let mut group = track.append_group().unwrap();
		group.write_frame(ts(5), b"key".to_vec()).unwrap();
		let mut received = recv(&mut subscriber).await;
		let frame = next(&mut received).await.unwrap();
		assert_eq!(
			(frame.timestamp, &frame.payload.contiguous(0)[..]),
			(Some(ts(5)), &b"key"[..])
		);
		assert!(frame.fields.is_none() && received.wire().is_none());
		group.write_frame(ts(6), b"delta".to_vec()).unwrap();
		group.finish().unwrap();
		let frame = next(&mut received).await.unwrap();
		assert_eq!(
			(frame.timestamp, &frame.payload.contiguous(0)[..]),
			(Some(ts(6)), &b"delta"[..])
		);
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

	/// Only a negotiated hop can preserve several datagram Object IDs. A late or lost
	/// object 0 never lets another object's payload pose as the whole group.
	#[moq_clock::test]
	async fn datagram_objects_cross_only_an_identifying_hop() {
		for offer in [[false, false], [false, true], [true, false], [true, true]] {
			for objects in [&[0, 1, 2][..], &[1, 2, 0], &[1, 2]] {
				let negotiated = offer == [true, true];
				let upstream = crate::origin::Config::new(crate::Hop::new(2).unwrap()).produce();
				let broadcast = upstream.publish("room", crate::origin::Route::default()).unwrap();
				let info = track::Info {
					properties: draft22(Vec::new()),
					..track::Info::timed().with_timescale(Timescale::MILLI)
				};
				let mut track = broadcast.create_track("video", info).unwrap();
				let (downstream, sessions) = test_hop(&upstream, "room", offer).await;
				let (_copy, mut subscriber) = subscribe(&downstream).await;
				// Receiving a group proves the subscription and its alias are bound before
				// the datagrams, independently of how the session tasks are scheduled.
				track.write_frame(ts(0), b"before".as_slice()).unwrap();
				let mut before = recv(&mut subscriber).await;
				assert_eq!(&next(&mut before).await.unwrap().payload.contiguous(0)[..], b"before");
				for &object in objects {
					track
						.insert_wire_datagram(crate::model::datagram::Stored {
							datagram: crate::Datagram {
								sequence: 1,
								timestamp: Some(ts(1)),
								payload: bytes::Bytes::from(vec![object as u8]),
							},
							wire: Some(Arc::new(crate::model::datagram::Wire {
								object_id: Some(object),
								priority: Some(200),
								end_of_group: object == 2,
								status: None,
								properties: Default::default(),
							})),
							deflated: None,
						})
						.unwrap();
				}
				track.insert_datagram(2, ts(2), b"next".as_slice()).unwrap();
				let mut seen = Vec::new();
				loop {
					let datagram = kio::wait(|waiter| subscriber.poll_recv_wire_datagram(waiter))
						.await
						.unwrap()
						.unwrap();
					if datagram.sequence == 2 {
						assert_eq!(&datagram.payload[..], b"next");
						assert!(datagram.wire.is_none());
						break;
					}
					assert_eq!(datagram.sequence, 1);
					assert_eq!(datagram.payload.len(), 1);
					let object = datagram.payload[0] as u64;
					if negotiated {
						let wire = datagram.wire.as_ref().unwrap();
						assert_eq!(wire.object_id, Some(object));
						assert_eq!(wire.priority, Some(200));
						assert_eq!(wire.end_of_group, object == 2);
					} else {
						assert!(datagram.wire.is_none());
					}
					seen.push(object);
				}
				let expected: Vec<_> = objects.iter().copied().filter(|id| negotiated || *id == 0).collect();
				assert_eq!(seen, expected, "offer={offer:?} arrivals={objects:?}");
				track.write_frame(ts(3), b"after".as_slice()).unwrap();
				let mut after = recv(&mut subscriber).await;
				assert_eq!(after.sequence, 3);
				assert_eq!(&next(&mut after).await.unwrap().payload.contiguous(0)[..], b"after");
				drop(sessions);
			}
		}
	}

	/// Provenance keeps IETF times unchecked after a hop, while a peer declining the
	/// extension gets no timeline. SUBSCRIBE, FETCH, and datagrams must agree with TRACK_INFO,
	/// while a listing stays untimed.
	#[moq_clock::test]
	async fn ietf_timestamps_keep_their_provenance_across_a_hop() {
		for offer in [[true, true], [true, false], [false, true]] {
			let negotiated = offer == [true, true];
			let upstream = crate::origin::Config::new(crate::Hop::new(2).unwrap()).produce();
			let broadcast = upstream.publish("room", crate::origin::Route::default()).unwrap();
			let info = track::Info {
				properties: draft22(Vec::new()),
				..track::Info::timed()
			};
			let mut track = broadcast.create_track("video", info).unwrap();
			let mut head = track.create_group(0u64.into()).unwrap();
			head.write_frame(ts(100), b"frame".to_vec()).unwrap();
			head.finish().unwrap();
			let (downstream, sessions) = test_hop(&upstream, "room", offer).await;
			let broadcast = downstream.consume().request("room", None).await.unwrap();
			// A listing cannot carry the provenance, so it lists the track untimed even when
			// negotiated: a relay re-listing it would otherwise advertise it as native lite.
			let mut tracks = broadcast.tracks().unwrap();
			let listed = loop {
				match moq_clock::timeout(TIMEOUT, tracks.next()).await.unwrap().unwrap() {
					Some(crate::broadcast::TracksEvent::Added { name, info }) if &*name == "video" => break info,
					Some(_) => continue,
					None => panic!("listing ended before the track"),
				}
			};
			assert_eq!((listed.timescale, listed.ietf()), (None, None), "{offer:?}");
			let copy = broadcast.track("video").unwrap();
			let info = moq_clock::timeout(TIMEOUT, copy.info()).await.unwrap().unwrap();
			assert_eq!(info.timescale, negotiated.then_some(Timescale::MILLI), "{offer:?}");
			assert_eq!(
				info.ietf(),
				negotiated.then_some(crate::ietf::Version::Draft22),
				"{offer:?}"
			);
			// Before any subscription, this must fetch the head over the hop, rather
			// than reading an already subscribed copy's cache.
			let mut fetched = moq_clock::timeout(TIMEOUT, copy.fetch_group(0, None))
				.await
				.unwrap()
				.unwrap();
			assert_eq!(next(&mut fetched).await.unwrap().timestamp, negotiated.then(|| ts(100)));
			assert!(next(&mut fetched).await.is_none());
			let mut subscriber =
				moq_clock::timeout(TIMEOUT, copy.subscribe(track::Subscription::default().with_start(1)))
					.await
					.unwrap()
					.unwrap();
			// A peer may have stamped arrival times. The second group's rewind must not
			// acquire native moq-lite checks merely because it crossed a relay hop.
			let mut group = track.create_group(1u64.into()).unwrap();
			group.write_frame(ts(50), b"frame".to_vec()).unwrap();
			group.finish().unwrap();
			let mut received = recv(&mut subscriber).await;
			assert_eq!(received.sequence, 1);
			let frame = next(&mut received).await.unwrap();
			assert_eq!(frame.timestamp, negotiated.then(|| ts(50)), "{offer:?}");
			assert_eq!(&frame.payload.contiguous(0)[..], b"frame");
			assert!(next(&mut received).await.is_none());
			track.insert_datagram(2, ts(25), b"datagram".to_vec()).unwrap();
			let datagram = moq_clock::timeout(TIMEOUT, kio::wait(|waiter| subscriber.poll_recv_wire_datagram(waiter)))
				.await
				.unwrap()
				.unwrap()
				.unwrap();
			assert_eq!(datagram.timestamp, negotiated.then(|| ts(25)), "{offer:?}");
			assert_eq!(&datagram.payload[..], b"datagram");
			drop(sessions);
		}
	}

	/// A compressed track crosses a negotiated hop as stored: each frame's Inflated Size
	/// follows its Object Record, and a datagram's follows its Datagram State.
	#[moq_clock::test]
	async fn compressed_media_crosses_a_negotiated_hop() {
		let upstream = crate::origin::Config::new(crate::Hop::new(2).unwrap()).produce();
		let broadcast = upstream.publish("room", crate::origin::Route::default()).unwrap();
		let info = track::Info::timed()
			.with_timescale(Timescale::MILLI)
			.with_compress(true);
		let mut track = broadcast.create_track("video", info).unwrap();
		let (downstream, sessions) = test_hop(&upstream, "room", [true, true]).await;
		let (_copy, mut subscriber) = subscribe(&downstream).await;
		assert!(subscriber.info().compress);

		let payload = |n: u8| vec![n; 300];
		let mut group = track.append_group().unwrap();
		group.write_frame(ts(5), payload(1)).unwrap();
		group.write_frame(ts(6), payload(2)).unwrap();
		group.finish().unwrap();
		let mut received = recv(&mut subscriber).await;
		for (millis, n) in [(5, 1), (6, 2)] {
			let frame = next(&mut received).await.unwrap();
			assert_eq!(
				(frame.timestamp, &frame.payload.contiguous(0)[..]),
				(Some(ts(millis)), &payload(n)[..])
			);
			assert!(frame.fields.is_none());
		}
		assert!(next(&mut received).await.is_none());

		track.insert_datagram(7, ts(9), payload(3)).unwrap();
		let datagram = moq_clock::timeout(
			TIMEOUT,
			kio::wait(|waiter| subscriber.poll_recv_stored_datagram(waiter)),
		)
		.await
		.expect("no datagram")
		.unwrap()
		.unwrap();
		assert_eq!((datagram.sequence, datagram.timestamp), (7, Some(ts(9))));
		assert_eq!(&datagram.payload[..], &payload(3)[..]);
		assert!(datagram.deflated.is_some() && datagram.wire.is_none());
		drop(sessions);
	}
}
