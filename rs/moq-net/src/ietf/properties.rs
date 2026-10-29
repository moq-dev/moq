/// Track Properties: relay-visible metadata attached to tracks.
///
/// SUBSCRIBE_OK, PUBLISH, and FETCH_OK carry Track Properties.
/// They appear after the message parameters as a sequence of Key-Value-Pairs
/// (same delta-encoded format) until the end of the message.
///
/// Unlike Message Parameters which have a count prefix, Track Properties
/// have no count and are read until the end of the message payload.
///
/// MAX_CACHE_DURATION, TIMESCALE, DEFAULT_PUBLISHER_PRIORITY, DEFAULT_PUBLISHER_GROUP_ORDER,
/// DYNAMIC_GROUPS, and COMPRESSION are understood; the rest are kept so a relay can forward them.
use std::time::Duration;

use bytes::Bytes;

use crate::Timescale;
use crate::coding::{DecodeError, Decoder, EncodeError, Encoder};
use crate::model::object::{Property, TrackProperties, Value};

use super::{GroupOrder, Version};

/// The most Track Properties a message may carry, which also bounds what a relay forwards.
pub(crate) const MAX_PROPERTIES: u64 = 64;
/// Maximum byte value length per spec Section 1.4.3.
const MAX_KVP_VALUE_LEN: usize = (1 << 16) - 1;

/// MAX_CACHE_DURATION (0x04): how long a relay may serve an object from its cache.
const MAX_CACHE_DURATION: u64 = 0x04;

/// TIMESCALE (0x08), from the MOQ Properties registry shared with draft-ietf-moq-loc-04.
///
/// Track scope: it declares the units of every object Timestamp on the track, and its
/// presence is what opts the track into timestamps at all.
pub(super) const TIMESCALE: u64 = 0x08;

/// DEFAULT_PUBLISHER_GROUP_ORDER (0x22), the publisher's delivery preference for the track.
///
/// It shares its number with the GROUP_ORDER *message parameter*, which is a different
/// registry: that one is only legal in SUBSCRIBE, PUBLISH_OK, and FETCH, where the
/// subscriber states its own preference.
const DEFAULT_PUBLISHER_PRIORITY: u64 = 0x0e;
const DEFAULT_PUBLISHER_GROUP_ORDER: u64 = 0x22;

/// DYNAMIC_GROUPS (0x30): a subscriber may ask the original publisher for a new group with
/// NEW_GROUP_REQUEST.
const DYNAMIC_GROUPS: u64 = 0x30;

/// Immutable Properties (0x0b): Key-Value-Pairs the original publisher sealed, which a
/// processor searches like the mutable list.
const IMMUTABLE: u64 = 0x0b;

/// The Track Properties block carried at the end of SUBSCRIBE_OK, PUBLISH, and FETCH_OK.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct Properties {
	/// The publisher's cache window; omission leaves retention unconstrained.
	pub max_cache_duration: Option<Duration>,

	/// The track's Timescale, which declares the units of every object Timestamp on it.
	///
	/// `None` declares no timeline, so the subscriber times objects by arrival.
	pub timescale: Option<Timescale>,

	/// Publisher priority for groups whose header omits it. Draft-22 section 10.4
	/// assigns property 0x0e and defaults to wire priority 128 when absent.
	pub priority: Option<u8>,

	/// The publisher's preference for prioritizing groups within a subscription.
	///
	/// `None` means the draft default, Ascending.
	pub group_order: Option<GroupOrder>,

	/// Whether the track takes NEW_GROUP_REQUEST (DYNAMIC_GROUPS = 1), mutable or sealed.
	pub dynamic_groups: bool,

	/// Whether each subgroup carries the negotiated group-scoped DEFLATE codec.
	pub compress: bool,

	/// Everything else, Immutable Properties included, forwarded as the publisher sent it,
	/// serialized as the message's draft serializes them.
	pub(crate) unknown: Vec<Property>,
}

impl Properties {
	/// The properties a relay states for a track it serves, from the `info` it acts on.
	///
	/// We serve the newest group first by default, matching moq-lite.
	pub(crate) fn restated(info: &crate::track::Info) -> Self {
		Self {
			max_cache_duration: info.max_age,
			timescale: info.timescale,
			priority: Some(super::priority::to_wire(info.priority)),
			group_order: Some(GroupOrder::Descending),
			// A track opted in to refresh answers a NEW_GROUP_REQUEST as a join.
			dynamic_groups: info.refresh.is_some(),
			compress: info.compress,
			unknown: Vec::new(),
		}
	}

	/// Write the block, which is the final field of the message: no count and no length,
	/// so the caller must not append anything after it.
	///
	/// Properties are serialized in ascending order by type, delta-encoded.
	///
	/// A property the publisher sealed inside Immutable Properties travels there, so it is
	/// not restated in the mutable list.
	pub fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		let int = |id, value| Property {
			id,
			value: Value::Int(value),
		};
		let mut known = Vec::new();

		if let Some(age) = self.max_cache_duration {
			let millis = u64::try_from(age.as_millis()).map_err(|_| EncodeError::BoundsExceeded)?;
			known.push(int(MAX_CACHE_DURATION, millis));
		}
		if let Some(timescale) = self.timescale {
			known.push(int(TIMESCALE, u64::from(timescale)));
		}
		if let Some(priority) = self.priority {
			known.push(int(DEFAULT_PUBLISHER_PRIORITY, u64::from(priority)));
		}
		if let Some(group_order) = self.group_order {
			known.push(int(DEFAULT_PUBLISHER_GROUP_ORDER, u64::from(u8::from(group_order))));
		}

		// Omitted means 0, so only support is worth bytes.
		if self.dynamic_groups {
			known.push(int(DYNAMIC_GROUPS, 1));
		}

		if self.compress {
			known.push(int(super::flate::COMPRESSION, 1));
		}

		let mut properties: Vec<_> = known
			.iter()
			.filter(|property| !sealed_keys(&self.unknown, version).any(|id| id == property.id))
			.chain(&self.unknown)
			.collect();
		// Delta types require ascending order. Equal types keep the publisher's order.
		properties.sort_by_key(|property| property.id);

		let mut prev_type = 0;
		for property in properties {
			w.varint(property.id - prev_type)?;
			prev_type = property.id;
			match &property.value {
				Value::Int(value) => w.varint(*value)?,
				Value::Bytes(value) => w.bytes(value)?,
			}
		}

		Ok(())
	}

	/// Parse Track Properties from the remaining bytes of a message.
	///
	/// Track Properties use the same Key-Value-Pair encoding as parameters:
	/// delta-encoded types, even = varint value, odd = length-prefixed bytes.
	/// They have no count prefix. Read until the buffer is empty.
	///
	/// Unlike an unknown message parameter, an unknown property is kept rather than
	/// fatal, which is what lets a relay forward properties it does not implement.
	pub fn decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let mut properties = Self::default();

		let mut prev_type: u64 = 0;
		let mut i: u64 = 0;
		let mut seen = Vec::new();

		while !r.is_empty() {
			if i >= MAX_PROPERTIES {
				return Err(DecodeError::TooMany);
			}

			let delta = r.varint()?;
			let abs = if i == 0 {
				delta
			} else {
				prev_type.checked_add(delta).ok_or(DecodeError::BoundsExceeded)?
			};
			prev_type = abs;
			i += 1;

			if abs % 2 == 0 {
				// Even type: single varint value
				let value = r.varint()?;
				if !properties.known(abs, value, &mut seen)? {
					properties.unknown.push(Property {
						id: abs,
						value: Value::Int(value),
					});
				}
			} else {
				// Odd type: length-prefixed bytes
				let len = usize::try_from(r.varint()?).map_err(|_| DecodeError::BoundsExceeded)?;
				if len > MAX_KVP_VALUE_LEN {
					return Err(DecodeError::BoundsExceeded);
				}
				let value = r.slice(len)?;
				if abs == IMMUTABLE {
					// A second block would leave the sealed list ambiguous.
					if immutable(&properties.unknown).is_some() {
						return Err(DecodeError::InvalidValue);
					}
					sealed(&mut Decoder::new(value, version.into()), &mut properties, &mut seen)?;
				}
				// A small kept property must not pin the message's larger buffer.
				properties.unknown.push(Property {
					id: abs,
					value: Value::Bytes(Bytes::copy_from_slice(value)),
				});
			}
		}

		Ok(properties)
	}

	/// Apply a property we understand, returning `false` for any other. Each holds one
	/// value and is searched in both lists, so a second occurrence, mutable or sealed, is
	/// malformed: forwarding the sealed one would contradict the one we act on.
	fn known(&mut self, id: u64, value: u64, seen: &mut Vec<u64>) -> Result<bool, DecodeError> {
		match id {
			MAX_CACHE_DURATION => self.max_cache_duration = Some(Duration::from_millis(value)),
			TIMESCALE => {
				// A zero timescale is invalid; treat it as no declaration rather than
				// failing the whole message over one property we could have ignored.
				self.timescale = Timescale::new(value).ok();
			}
			DEFAULT_PUBLISHER_PRIORITY => {
				self.priority = Some(u8::try_from(value).map_err(|_| DecodeError::InvalidValue)?);
			}
			DEFAULT_PUBLISHER_GROUP_ORDER => {
				// Only Ascending and Descending are defined: 0x0 has no "publisher
				// decides" meaning to fall back on.
				self.group_order = match value {
					1 => Some(GroupOrder::Ascending),
					2 => Some(GroupOrder::Descending),
					_ => return Err(DecodeError::InvalidValue),
				};
			}
			DYNAMIC_GROUPS => self.dynamic_groups = dynamic_groups(value)?,
			super::flate::COMPRESSION => {
				if value != 1 {
					return Err(DecodeError::InvalidValue);
				}
				self.compress = true;
			}
			_ => return Ok(false),
		}
		if seen.contains(&id) {
			return Err(DecodeError::InvalidValue);
		}
		seen.push(id);
		Ok(true)
	}
}

/// The Track Properties moq-net applies itself, which a relay restates from the track's
/// info rather than forwarding the publisher's.
const UNDERSTOOD: [u64; 6] = [
	MAX_CACHE_DURATION,
	TIMESCALE,
	DEFAULT_PUBLISHER_PRIORITY,
	DEFAULT_PUBLISHER_GROUP_ORDER,
	DYNAMIC_GROUPS,
	super::flate::COMPRESSION,
];

/// Whether moq-net applies the Track Property `id` itself.
pub(crate) fn understood(id: u64) -> bool {
	UNDERSTOOD.contains(&id)
}

/// The most Track Properties a relay forwards, leaving room for every one it restates
/// within the [`MAX_PROPERTIES`] a message may carry.
pub(crate) const MAX_FORWARDED: usize = MAX_PROPERTIES as usize - UNDERSTOOD.len();

/// Check a Track Properties' Immutable Properties `block` from a `version` publisher, as
/// [`Properties::decode`] does.
pub(crate) fn check_immutable(block: &[u8], version: Version) -> Result<(), DecodeError> {
	sealed(
		&mut Decoder::new(block, version.into()),
		&mut Properties::default(),
		&mut Vec::new(),
	)
}

/// The Immutable Properties block among `values`.
fn immutable(values: &[Property]) -> Option<&Bytes> {
	values.iter().find_map(|property| match (property.id, &property.value) {
		(IMMUTABLE, Value::Bytes(block)) => Some(block),
		_ => None,
	})
}

/// The keys inside the Immutable Properties among `values`, serialized as `version` does.
///
/// Read from the block each time rather than held as a list, which could be many times its
/// size. Ingress checked the block, so a malformed key cannot occur and ends it.
fn sealed_keys(values: &[Property], version: Version) -> impl Iterator<Item = u64> + '_ {
	let mut r = immutable(values).map(|block| Decoder::new(block, version.into()));
	let mut id: u64 = 0;
	std::iter::from_fn(move || {
		let r = r.as_mut().filter(|r| !r.is_empty())?;
		id = id.checked_add(r.varint().ok()?)?;
		match id.is_multiple_of(2) {
			true => r.varint().ok().map(drop)?,
			false => r.bytes().ok().map(drop)?,
		}
		Some(id)
	})
}

impl TrackProperties {
	/// The Immutable Properties block, serialized as [`Self::version`] does.
	pub(crate) fn immutable(&self) -> Option<&Bytes> {
		immutable(&self.values)
	}

	/// The keys inside Immutable Properties, which egress must not restate as mutable.
	pub(crate) fn sealed(&self) -> impl Iterator<Item = u64> + '_ {
		sealed_keys(&self.values, self.version)
	}

	/// Why a track carrying these beside `info` is neither processed nor forwarded, if it
	/// is not: every ingress, moq-transport or a relay hop, refuses it here.
	pub(crate) fn refused(&self, info: &crate::track::Info) -> Option<crate::Error> {
		if self.mandatory() {
			return Some(crate::Error::UnsupportedExtension);
		}
		// Egress forwards a sealed value in place of restating `info`, so the two must agree.
		if self.contradicts(&Properties::restated(info)) {
			return Some(crate::Error::Unsupported);
		}
		// Egress could not restate the track's info beside them without overflowing.
		if self.values.len() > MAX_FORWARDED {
			return Some(crate::Error::Unsupported);
		}
		// Nor could a relay hop carry them in one message.
		if !crate::lite::ietf::fits(self) {
			return Some(crate::Error::Unsupported);
		}
		None
	}

	/// Whether Immutable Properties seal a property we understand at another value than
	/// the `restated` one the relay acts on.
	fn contradicts(&self, restated: &Properties) -> bool {
		let Some(block) = self.immutable() else {
			return false;
		};
		let mut sealed = Properties::default();
		let mut seen = Vec::new();
		let r = &mut Decoder::new(block, self.version.into());
		// Ingress checked the block, so a malformed one cannot occur and is refused.
		if self::sealed(r, &mut sealed, &mut seen).is_err() {
			return true;
		}
		seen.into_iter().any(|id| match id {
			MAX_CACHE_DURATION => sealed.max_cache_duration != restated.max_cache_duration,
			TIMESCALE => sealed.timescale != restated.timescale,
			DEFAULT_PUBLISHER_PRIORITY => sealed.priority != restated.priority,
			// Advisory: a relay serves its own order and forwards the publisher's preference.
			DEFAULT_PUBLISHER_GROUP_ORDER => false,
			DYNAMIC_GROUPS => sealed.dynamic_groups != restated.dynamic_groups,
			super::flate::COMPRESSION => sealed.compress != restated.compress,
			_ => unreachable!("only the understood properties are seen"),
		})
	}

	/// Whether one is a Mandatory Track Property (0x4000-0x7FFF), which by definition is
	/// one moq-net does not understand: the track must be neither processed nor forwarded.
	fn mandatory(&self) -> bool {
		const MANDATORY: std::ops::RangeInclusive<u64> = 0x4000..=0x7fff;
		self.values.iter().any(|property| MANDATORY.contains(&property.id))
			|| self.sealed().any(|id| MANDATORY.contains(&id))
	}
}

/// A DYNAMIC_GROUPS value, which is a boolean: anything but 0 or 1 is malformed.
pub(super) fn dynamic_groups(value: u64) -> Result<bool, DecodeError> {
	match value {
		0 => Ok(false),
		1 => Ok(true),
		_ => Err(DecodeError::InvalidValue),
	}
}

/// Check the keys inside an Immutable Properties value, applying the ones we understand,
/// which the draft makes a processor search too. Its pairs are delta-encoded like the outer
/// list, and nesting another Immutable Properties makes the track malformed.
fn sealed(r: &mut Decoder<'_>, properties: &mut Properties, seen: &mut Vec<u64>) -> Result<(), DecodeError> {
	let mut id: u64 = 0;
	while !r.is_empty() {
		id = id.checked_add(r.varint()?).ok_or(DecodeError::BoundsExceeded)?;
		if id.is_multiple_of(2) {
			properties.known(id, r.varint()?, seen)?;
		} else {
			if id == IMMUTABLE {
				return Err(DecodeError::InvalidValue);
			}
			r.bytes()?;
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_skip_empty_properties() {
		let mut buf = bytes::Bytes::new();
		assert_eq!(
			crate::coding::decode_buf(&mut buf, Version::Draft18, Properties::decode).unwrap(),
			Properties::default()
		);
	}

	#[test]
	fn test_skip_varint_property() {
		// Even type (0x02 = DELIVERY_TIMEOUT), varint value
		let mut buf = Vec::new();
		Encoder::new(&mut buf, Version::Draft18.into()).varint(0x02u64).unwrap(); // delta type
		Encoder::new(&mut buf, Version::Draft18.into()).varint(5000u64).unwrap(); // value
		let mut bytes = bytes::Bytes::from(buf);
		crate::coding::decode_buf(&mut bytes, Version::Draft18, Properties::decode).unwrap();
		assert!(bytes.is_empty());
	}

	#[test]
	fn test_skip_bytes_property() {
		// Odd type (0x0B = IMMUTABLE_PROPERTIES), length-prefixed
		let mut buf = Vec::new();
		Encoder::new(&mut buf, Version::Draft18.into()).varint(0x0Bu64).unwrap(); // delta type
		Encoder::new(&mut buf, Version::Draft18.into()).varint(3u64).unwrap(); // length
		buf.extend_from_slice(&[0x03, 0x01, 0x2A]); // sealed type 0x03, one byte
		let mut bytes = bytes::Bytes::from(buf);
		crate::coding::decode_buf(&mut bytes, Version::Draft18, Properties::decode).unwrap();
		assert!(bytes.is_empty());
	}

	#[test]
	fn test_skip_multiple_properties() {
		let mut buf = Vec::new();
		// First: type 0x02 (even), varint value
		Encoder::new(&mut buf, Version::Draft18.into()).varint(0x02u64).unwrap();
		Encoder::new(&mut buf, Version::Draft18.into()).varint(1000u64).unwrap();
		// Second: delta = 0x02 → abs type 0x04 (even), varint value
		Encoder::new(&mut buf, Version::Draft18.into()).varint(0x02u64).unwrap();
		Encoder::new(&mut buf, Version::Draft18.into()).varint(2000u64).unwrap();
		// Third: delta = 0x07 → abs type 0x0B (odd), length-prefixed
		Encoder::new(&mut buf, Version::Draft18.into()).varint(0x07u64).unwrap();
		Encoder::new(&mut buf, Version::Draft18.into()).varint(2u64).unwrap();
		buf.extend_from_slice(&[0x02, 0x2A]); // sealed type 0x02 = 42

		let mut bytes = bytes::Bytes::from(buf);
		crate::coding::decode_buf(&mut bytes, Version::Draft18, Properties::decode).unwrap();
		assert!(bytes.is_empty());
	}

	#[test]
	fn test_round_trip() {
		let properties = Properties {
			unknown: Default::default(),
			max_cache_duration: None,
			timescale: Some(Timescale::MICRO),
			priority: Some(37),
			group_order: Some(GroupOrder::Descending),
			dynamic_groups: true,
			compress: false,
		};

		let mut buf = Vec::new();
		properties
			.encode(&mut Encoder::new(&mut buf, Version::Draft18.into()), Version::Draft18)
			.unwrap();

		let mut bytes = bytes::Bytes::from(buf);
		assert_eq!(
			crate::coding::decode_buf(&mut bytes, Version::Draft18, Properties::decode).unwrap(),
			properties
		);
		assert!(bytes.is_empty());
	}

	#[test]
	fn publisher_priority_is_bounded_to_a_byte() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let mut buf = Vec::new();
			Encoder::new(&mut buf, version.into()).varint(0x0eu64).unwrap();
			Encoder::new(&mut buf, version.into()).varint(256u64).unwrap();
			assert!(matches!(
				crate::coding::decode_buf(&mut bytes::Bytes::from(buf), version, Properties::decode),
				Err(DecodeError::InvalidValue)
			));
		}
	}

	/// Only Ascending and Descending are defined, so 0x0 is a malformed message rather than
	/// a "publisher decides".
	#[test]
	fn test_rejects_zero_group_order() {
		let mut buf = Vec::new();
		Encoder::new(&mut buf, Version::Draft18.into()).varint(0x22u64).unwrap();
		Encoder::new(&mut buf, Version::Draft18.into()).varint(0).unwrap();

		let mut bytes = bytes::Bytes::from(buf);
		assert!(crate::coding::decode_buf(&mut bytes, Version::Draft18, Properties::decode).is_err());
	}

	/// The group order property is delta-encoded against the timescale that precedes it,
	/// so it has to survive the timescale being absent.
	#[test]
	fn test_round_trip_group_order_only() {
		let properties = Properties {
			unknown: Default::default(),
			dynamic_groups: false,
			compress: false,
			max_cache_duration: None,
			timescale: None,
			priority: None,
			group_order: Some(GroupOrder::Descending),
		};

		let mut buf = Vec::new();
		properties
			.encode(&mut Encoder::new(&mut buf, Version::Draft18.into()), Version::Draft18)
			.unwrap();

		let mut bytes = bytes::Bytes::from(buf);
		assert_eq!(
			crate::coding::decode_buf(&mut bytes, Version::Draft18, Properties::decode).unwrap(),
			properties
		);
		assert!(bytes.is_empty());
	}

	fn decode_draft22(pairs: &[u8]) -> Result<Properties, DecodeError> {
		crate::coding::decode_buf(
			&mut bytes::Bytes::from(pairs.to_vec()),
			Version::Draft22,
			Properties::decode,
		)
	}

	/// DYNAMIC_GROUPS holds 0 or 1, and the draft closes the session over anything else.
	#[test]
	fn dynamic_groups_is_a_boolean() {
		assert!(decode_draft22(&[0x30, 0x01]).unwrap().dynamic_groups);
		assert!(!decode_draft22(&[0x30, 0x00]).unwrap().dynamic_groups);
		assert!(!decode_draft22(&[]).unwrap().dynamic_groups);
		assert!(matches!(decode_draft22(&[0x30, 0x02]), Err(DecodeError::InvalidValue)));
	}

	/// A processor searches Immutable Properties too, with the same validation.
	#[test]
	fn dynamic_groups_inside_immutable_properties() {
		// 0x0b, length 2: { 0x30 = 1 }
		assert!(decode_draft22(&[0x0b, 0x02, 0x30, 0x01]).unwrap().dynamic_groups);
		assert!(matches!(
			decode_draft22(&[0x0b, 0x02, 0x30, 0x02]),
			Err(DecodeError::InvalidValue)
		));
		// Immutable Properties nested inside Immutable Properties is malformed.
		assert!(decode_draft22(&[0x0b, 0x02, 0x0b, 0x00]).is_err());
		// One value: the property cannot appear both mutable and sealed.
		assert!(decode_draft22(&[0x0b, 0x02, 0x30, 0x01, 0x25, 0x01]).is_err());
	}

	/// Only support is written: an absent property already means 0.
	#[test]
	fn dynamic_groups_is_written_only_when_supported() {
		let mut buf = Vec::new();
		Properties {
			dynamic_groups: true,
			compress: false,
			..Default::default()
		}
		.encode(&mut Encoder::new(&mut buf, Version::Draft22.into()), Version::Draft22)
		.unwrap();
		assert_eq!(buf, [0x30, 0x01]);

		let mut buf = Vec::new();
		Properties::default()
			.encode(&mut Encoder::new(&mut buf, Version::Draft22.into()), Version::Draft22)
			.unwrap();
		assert!(buf.is_empty());
	}

	fn round_trip(version: Version, block: &[u8]) -> (Properties, Vec<u8>) {
		let properties = crate::coding::decode_buf(&mut Bytes::copy_from_slice(block), version, |r, v| {
			Properties::decode(r, v)
		})
		.unwrap_or_else(|err| panic!("{version}: {err}"));
		let mut buf = Vec::new();
		properties
			.encode(&mut Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		(properties, buf)
	}

	/// Unknown properties, Immutable Properties among them, are written back as they arrived,
	/// in order among the ones we write ourselves.
	#[test]
	fn unknown_properties_round_trip() {
		let version = Version::Draft22;
		// 0x02 = 5000, 0x0b = { 0x02 = 42 }, 0x3801 = "hi"
		let mut block = Vec::new();
		let mut w = Encoder::new(&mut block, version.into());
		w.varint(0x02).unwrap();
		w.varint(5000).unwrap();
		w.varint(0x09).unwrap();
		w.bytes(&[0x02, 42]).unwrap();
		w.varint(0x3801 - 0x0b).unwrap();
		w.bytes(b"hi").unwrap();

		let (properties, written) = round_trip(version, &block);
		assert_eq!(written, block, "{version}");
		assert_eq!(properties.unknown.len(), 3, "{version}");
		let track = forwarded(&properties, version);
		assert!(track.sealed().eq([0x02]), "{version}");
		assert!(!track.mandatory(), "{version}");

		// Ours interleave by type.
		let mut ours = properties.clone();
		ours.dynamic_groups = true;
		let mut buf = Vec::new();
		ours.encode(&mut Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		let decoded = crate::coding::decode_buf(&mut Bytes::from(buf), version, Properties::decode).unwrap();
		assert_eq!(decoded, ours, "{version}");
	}

	/// A property sealed inside Immutable Properties is not restated in the mutable list.
	#[test]
	fn sealed_properties_are_not_restated() {
		let block = [0x0b, 0x02, 0x30, 0x01];
		let (properties, written) = round_trip(Version::Draft22, &block);
		assert!(properties.dynamic_groups);
		assert_eq!(written, block);
	}

	/// A sealed property we understand is applied, and stating it in both lists is malformed:
	/// we would act on one value and forward the other.
	#[test]
	fn sealed_known_properties_are_applied_once() {
		let mut sealed = Vec::new();
		let mut w = Encoder::new(&mut sealed, Version::Draft22.into());
		w.varint(TIMESCALE).unwrap();
		w.varint(1000).unwrap();
		let mut block = Vec::new();
		let mut w = Encoder::new(&mut block, Version::Draft22.into());
		w.varint(IMMUTABLE).unwrap();
		w.bytes(&sealed).unwrap();

		let (properties, written) = round_trip(Version::Draft22, &block);
		assert_eq!(properties.timescale, Some(Timescale::MILLI));
		assert_eq!(written, block);

		let mut both = Vec::new();
		let mut w = Encoder::new(&mut both, Version::Draft22.into());
		w.varint(TIMESCALE).unwrap();
		w.varint(90_000).unwrap();
		w.varint(IMMUTABLE - TIMESCALE).unwrap();
		w.bytes(&sealed).unwrap();
		assert!(matches!(decode_draft22(&both), Err(DecodeError::InvalidValue)));
	}

	/// The sealed list is unambiguous only with one Immutable Properties.
	#[test]
	fn a_second_immutable_block_is_malformed() {
		assert!(matches!(
			decode_draft22(&[0x0b, 0x00, 0x00, 0x00]),
			Err(DecodeError::InvalidValue)
		));
	}

	/// What a relay keeps of `properties`, which arrived on `version`.
	fn forwarded(properties: &Properties, version: Version) -> TrackProperties {
		TrackProperties {
			version,
			values: properties.unknown.clone(),
		}
	}

	/// A relay forwards only as many properties as leave room for every one it restates, so
	/// what it sends always decodes. One more is refused at ingress.
	#[test]
	fn forwarded_properties_leave_room_for_the_restated() {
		let unknown = |count: usize| TrackProperties {
			version: Version::Draft22,
			values: (0..count as u64)
				.map(|i| Property {
					id: 0x40 + 2 * i,
					value: Value::Int(1),
				})
				.collect(),
		};
		let properties = Properties {
			max_cache_duration: Some(Duration::from_secs(1)),
			timescale: Some(Timescale::MILLI),
			priority: Some(1),
			group_order: Some(GroupOrder::Descending),
			dynamic_groups: true,
			compress: false,
			unknown: unknown(MAX_FORWARDED).values,
		};
		assert!(
			forwarded(&properties, Version::Draft22)
				.refused(&crate::track::Info::default())
				.is_none()
		);
		let mut buf = Vec::new();
		properties
			.encode(&mut Encoder::new(&mut buf, Version::Draft22.into()), Version::Draft22)
			.unwrap();
		assert_eq!(decode_draft22(&buf).unwrap(), properties);

		assert!(matches!(
			unknown(MAX_FORWARDED + 1).refused(&crate::track::Info::default()),
			Some(crate::Error::Unsupported)
		));
	}

	/// Why a track whose publisher sealed `value` as the understood property `id` is
	/// refused beside an info that restates 7s, 1000 Hz, priority 9, Descending, and
	/// DYNAMIC_GROUPS.
	fn seal(id: u64, value: u64) -> Option<crate::Error> {
		let version = Version::Draft22;
		let mut block = Vec::new();
		let w = &mut Encoder::new(&mut block, version.into());
		w.varint(id).unwrap();
		w.varint(value).unwrap();
		let unknown = TrackProperties {
			version,
			values: vec![Property {
				id: IMMUTABLE,
				value: Value::Bytes(block.into()),
			}],
		};
		let info = crate::track::Info::default()
			.with_max_age(Duration::from_secs(7))
			.with_timescale(Timescale::MILLI)
			.with_priority(9)
			.with_refresh(0);
		unknown.refused(&info)
	}

	/// A sealed value of a property the relay restates must be the one it acts on, since
	/// egress forwards the sealed one in its place.
	#[test]
	fn a_sealed_max_cache_duration_matches_the_track() {
		assert!(seal(MAX_CACHE_DURATION, 7000).is_none());
		assert!(matches!(
			seal(MAX_CACHE_DURATION, 7001),
			Some(crate::Error::Unsupported)
		));
	}

	#[test]
	fn a_sealed_timescale_matches_the_track() {
		assert!(seal(TIMESCALE, 1000).is_none());
		assert!(matches!(seal(TIMESCALE, 900), Some(crate::Error::Unsupported)));
		// Zero declares no timeline, which contradicts one.
		assert!(matches!(seal(TIMESCALE, 0), Some(crate::Error::Unsupported)));
	}

	#[test]
	fn a_sealed_priority_matches_the_track() {
		let wire = u64::from(super::super::priority::to_wire(9));
		assert!(seal(DEFAULT_PUBLISHER_PRIORITY, wire).is_none());
		assert!(matches!(
			seal(DEFAULT_PUBLISHER_PRIORITY, wire + 1),
			Some(crate::Error::Unsupported)
		));
	}

	#[test]
	fn a_sealed_group_order_is_advisory() {
		// The relay serves the newest group first, but forwards the publisher's preference.
		for order in [1, 2] {
			assert!(seal(DEFAULT_PUBLISHER_GROUP_ORDER, order).is_none(), "{order}");
		}

		// Egress forwards the sealed block unchanged and restates no mutable order beside it.
		let version = Version::Draft22;
		let mut sealed = Vec::new();
		let w = &mut Encoder::new(&mut sealed, version.into());
		w.varint(DEFAULT_PUBLISHER_GROUP_ORDER).unwrap();
		w.varint(1).unwrap();
		let unknown = vec![Property {
			id: IMMUTABLE,
			value: Value::Bytes(sealed.clone().into()),
		}];
		let egress = Properties {
			unknown: unknown.clone(),
			..Properties::restated(&crate::track::Info::default())
		};
		let mut buf = Vec::new();
		egress
			.encode(&mut Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		let decoded = decode_draft22(&buf).unwrap();
		assert_eq!(decoded.group_order, Some(GroupOrder::Ascending));
		assert_eq!(decoded.unknown, unknown);
	}

	#[test]
	fn sealed_dynamic_groups_match_the_track() {
		assert!(seal(DYNAMIC_GROUPS, 1).is_none());
		assert!(matches!(seal(DYNAMIC_GROUPS, 0), Some(crate::Error::Unsupported)));
	}

	/// Every property the relay restates has a sealed check above.
	#[test]
	fn every_understood_property_is_checked_when_sealed() {
		assert_eq!(
			UNDERSTOOD,
			[
				MAX_CACHE_DURATION,
				TIMESCALE,
				DEFAULT_PUBLISHER_PRIORITY,
				DEFAULT_PUBLISHER_GROUP_ORDER,
				DYNAMIC_GROUPS,
				super::super::flate::COMPRESSION
			]
		);
	}

	/// 0x4000-0x7FFF are Mandatory Track Properties, mutable or sealed.
	#[test]
	fn mandatory_properties_are_found() {
		let mut mandatory = Vec::new();
		let mut w = Encoder::new(&mut mandatory, Version::Draft22.into());
		w.varint(0x4000).unwrap();
		w.varint(1).unwrap();
		let mandatory_in = |list: &[u8]| forwarded(&decode_draft22(list).unwrap(), Version::Draft22).mandatory();
		assert!(mandatory_in(&mandatory));

		let mut sealed = vec![0x0b, mandatory.len() as u8];
		sealed.extend(&mandatory);
		assert!(mandatory_in(&sealed));

		let mut below = Vec::new();
		let mut w = Encoder::new(&mut below, Version::Draft22.into());
		w.varint(0x3ffe).unwrap();
		w.varint(1).unwrap();
		assert!(!mandatory_in(&below));
	}
}
