//! The Location Filter carried by SUBSCRIBE, PUBLISH and REQUEST_UPDATE.

use bytes::Buf as _;

use crate::coding::{Decode, DecodeError, Encode, EncodeError};

use super::{Location, Param, Version};

/// Which Objects a subscription delivers.
///
/// A subscriber has not learned Largest Object when it sends SUBSCRIBE, so the live-edge
/// relative forms are the only ones it can name up front. [`Self::Absolute`] is for the
/// cases where the caller already knows the group it wants.
///
/// The variants are the shapes the wire can actually express, so a range that no draft can
/// encode (a relative start with an end) cannot be built. Ordering matches the wire: a
/// relative start is always open ended.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
	/// Every Object in the track, encoded by omitting the parameter.
	Unfiltered,

	/// The next Object after the live edge: `{Largest.Group, Largest.Object + 1}`.
	///
	/// Delivery can begin mid-group, so a subscriber that needs a decodable start pairs
	/// this with a fill (see [`super::Fill`]) or uses [`Self::Relative`] instead.
	#[default]
	NextObject,

	/// Start the given number of groups back from the next group, always open ended.
	///
	/// `{Largest.Group + 1 - groups, 0}`, so 0 is the next group and 1 is the current one.
	/// Only draft-20 can encode a value above 1; older drafts have a tag per case.
	Relative(u64),

	/// An absolute range, ending at `end` (inclusive) when one is set.
	Absolute {
		/// The first Location to deliver.
		start: Location,
		/// Where the range ends, inclusive. `None` leaves it open ended.
		end: Option<EndLocation>,
	},
}

/// The inclusive end of an absolute [`Filter`] range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndLocation {
	/// The last group to deliver, inclusive.
	pub group: u64,
	/// The last object in that group, inclusive. `None` includes the whole group.
	pub object: Option<u64>,
}

/// The tagged Filter Type of draft-19 and earlier.
mod tag {
	pub const NEXT_GROUP: u64 = 0x1;
	pub const LARGEST_OBJECT: u64 = 0x2;
	pub const ABSOLUTE_START: u64 = 0x3;
	pub const ABSOLUTE_RANGE: u64 = 0x4;
}

impl Filter {
	/// Whether this is draft-20 or newer.
	///
	/// Draft-20 replaced the Filter Type tag with up to four optional varints, where the
	/// number present selects the meaning, and added the parameters that ride alongside it.
	pub(crate) fn is_draft20(version: Version) -> bool {
		!matches!(
			version,
			Version::Draft14
				| Version::Draft15
				| Version::Draft16
				| Version::Draft17
				| Version::Draft18
				| Version::Draft19
		)
	}

	/// The absolute end group, given the start, or an error when the range runs backwards.
	fn end_delta(start: u64, end: u64) -> Result<u64, EncodeError> {
		end.checked_sub(start).ok_or(EncodeError::InvalidState)
	}

	/// Encode the draft-20 field list, without the enclosing length prefix.
	fn encode_fields<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match *self {
			// Zero fields. The draft defines an open ended absolute {0, 0} as equivalent, so
			// that spelling normalizes to this one rather than colliding with NextObject.
			Self::Unfiltered => {}
			Self::NextObject => {
				0u64.encode(w, version)?;
				0u64.encode(w, version)?;
			}
			Self::Relative(groups) => groups.encode(w, version)?,
			Self::Absolute {
				start: Location { group: 0, object: 0 },
				end: None,
			} => {}
			Self::Absolute { start, end } => {
				start.group.encode(w, version)?;
				start.object.encode(w, version)?;
				if let Some(end) = end {
					Self::end_delta(start.group, end.group)?.encode(w, version)?;
					if let Some(object) = end.object {
						object.encode(w, version)?;
					}
				}
			}
		}
		Ok(())
	}

	/// Decode the draft-20 field list, which the caller has already delimited.
	fn decode_fields<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let mut fields = Vec::with_capacity(4);
		while r.has_remaining() {
			if fields.len() == 4 {
				return Err(DecodeError::TrailingBytes);
			}
			fields.push(u64::decode(r, version)?);
		}

		Ok(match fields[..] {
			[] => Self::Unfiltered,
			[groups] => Self::Relative(groups),
			// Two zeroes is the Next Object spelling; anything else is an absolute start.
			[0, 0] => Self::NextObject,
			[group, object] => Self::Absolute {
				start: Location { group, object },
				end: None,
			},
			[group, object, delta] => Self::Absolute {
				start: Location { group, object },
				end: Some(EndLocation {
					group: group.checked_add(delta).ok_or(DecodeError::BoundsExceeded)?,
					object: None,
				}),
			},
			[group, object, delta, end_object] => Self::Absolute {
				start: Location { group, object },
				end: Some(EndLocation {
					group: group.checked_add(delta).ok_or(DecodeError::BoundsExceeded)?,
					object: Some(end_object),
				}),
			},
			_ => unreachable!("capped at 4 fields above"),
		})
	}

	/// Encode the draft-19 and earlier tag form, without the enclosing length prefix.
	fn encode_tag<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match *self {
			// No tag means "everything", which only the absolute spelling can say.
			Self::Unfiltered => {
				tag::ABSOLUTE_START.encode(w, version)?;
				Location::default().encode(w, version)?;
			}
			Self::NextObject => tag::LARGEST_OBJECT.encode(w, version)?,
			Self::Relative(0) => tag::NEXT_GROUP.encode(w, version)?,
			// Only draft-20 can name a start further back than the next group without
			// knowing Largest Object, so there is no honest tag to fall back to.
			Self::Relative(_) => return Err(EncodeError::Unsupported),
			Self::Absolute { start, end: None } => {
				tag::ABSOLUTE_START.encode(w, version)?;
				start.encode(w, version)?;
			}
			// Draft-19's AbsoluteRange ends on a group, so an object-bounded range has no
			// spelling. Refuse rather than widen the range the caller asked for.
			Self::Absolute {
				end: Some(EndLocation { object: Some(_), .. }),
				..
			} => return Err(EncodeError::Unsupported),
			Self::Absolute { start, end: Some(end) } => {
				tag::ABSOLUTE_RANGE.encode(w, version)?;
				start.encode(w, version)?;
				Self::end_delta(start.group, end.group)?.encode(w, version)?;
			}
		}
		Ok(())
	}

	/// Decode the draft-19 and earlier tag form.
	fn decode_tag<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		Ok(match u64::decode(r, version)? {
			tag::NEXT_GROUP => Self::Relative(0),
			tag::LARGEST_OBJECT => Self::NextObject,
			tag::ABSOLUTE_START => Self::Absolute {
				start: Location::decode(r, version)?,
				end: None,
			},
			tag::ABSOLUTE_RANGE => {
				let start = Location::decode(r, version)?;
				let delta = u64::decode(r, version)?;
				Self::Absolute {
					start,
					end: Some(EndLocation {
						group: start.group.checked_add(delta).ok_or(DecodeError::BoundsExceeded)?,
						object: None,
					}),
				}
			}
			_ => return Err(DecodeError::InvalidValue),
		})
	}
}

/// The inline form, used by draft-14 where the filter is message fields rather than a
/// parameter. Later drafts go through [`Param`] instead.
impl Encode<Version> for Filter {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.encode_tag(w, version)
	}
}

impl Decode<Version> for Filter {
	fn decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		Self::decode_tag(r, version)
	}
}

impl Param for Filter {
	/// An unfiltered subscription is the default, so it goes on the wire as an absent
	/// parameter rather than an empty one.
	fn param_present(&self) -> bool {
		!matches!(self, Self::Unfiltered)
	}

	fn param_encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		let mut buf = Vec::new();

		// Inner varints use the draft-15 leading-ones encoding on the drafts that predate
		// the switch, matching the other length-prefixed parameters.
		let sv = match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => Version::Draft15,
			_ => version,
		};

		if Self::is_draft20(version) {
			self.encode_fields(&mut buf, sv)?;
		} else {
			self.encode_tag(&mut buf, sv)?;
		}

		buf.encode(w, version)
	}

	fn param_decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let data = Vec::<u8>::decode(r, version)?;
		let mut buf = bytes::Bytes::from(data);
		let sv = match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => Version::Draft15,
			_ => version,
		};

		if Self::is_draft20(version) {
			// The field count is what carries the meaning, so the value is consumed whole.
			return Self::decode_fields(&mut buf, sv);
		}

		let filter = Self::decode_tag(&mut buf, sv)?;
		if buf.has_remaining() {
			return Err(DecodeError::TrailingBytes);
		}
		Ok(filter)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const OLD: Version = Version::Draft19;
	const NEW: Version = Version::Draft20;

	fn round_trip(filter: Filter, version: Version) -> Filter {
		let mut buf = Vec::new();
		filter.param_encode(&mut buf, version).expect("encode");
		let mut bytes = bytes::Bytes::from(buf);
		let decoded = Filter::param_decode(&mut bytes, version).expect("decode");
		assert!(!bytes.has_remaining(), "parameter left trailing bytes");
		decoded
	}

	/// The value bytes, without the length prefix a parameter carries.
	fn value(filter: Filter, version: Version) -> Vec<u8> {
		let mut buf = Vec::new();
		filter.param_encode(&mut buf, version).expect("encode");
		let mut bytes = bytes::Bytes::from(buf);
		Vec::<u8>::decode(&mut bytes, version).expect("length prefix")
	}

	#[test]
	fn draft20_field_counts() {
		// The number of fields present is what selects the meaning, so pin the bytes.
		assert_eq!(value(Filter::NextObject, NEW), vec![0x00, 0x00]);
		assert_eq!(value(Filter::Relative(0), NEW), vec![0x00]);
		assert_eq!(value(Filter::Relative(1), NEW), vec![0x01]);
		assert_eq!(
			value(
				Filter::Absolute {
					start: Location { group: 7, object: 3 },
					end: None
				},
				NEW
			),
			vec![0x07, 0x03]
		);
		// End is a delta from the start group, not an absolute group id.
		assert_eq!(
			value(
				Filter::Absolute {
					start: Location { group: 7, object: 3 },
					end: Some(EndLocation { group: 9, object: None })
				},
				NEW
			),
			vec![0x07, 0x03, 0x02]
		);
	}

	/// Draft-17 replaced QUIC's varint with a leading-1-bits one. They agree below 64, so a
	/// wrong codec stays invisible until group or object ids grow past that.
	#[test]
	fn draft20_uses_leading_ones_varints_above_63() {
		// 128 is 0x80_80 with leading ones, and 0x40_80 with the QUIC form.
		assert_eq!(value(Filter::Relative(128), NEW), vec![0x80, 0x80]);
		assert_eq!(round_trip(Filter::Relative(128), NEW), Filter::Relative(128));

		let wide = Filter::Absolute {
			start: Location { group: 300, object: 64 },
			end: None,
		};
		assert_eq!(round_trip(wide, NEW), wide);
	}

	/// The current group is what moq-lite means by joining a track, and draft-20 is the
	/// first version that can name it without knowing Largest Object.
	#[test]
	fn draft20_names_the_current_group() {
		assert_eq!(value(Filter::Relative(1), NEW), vec![0x01]);
		assert_eq!(round_trip(Filter::Relative(1), NEW), Filter::Relative(1));
	}

	#[test]
	fn draft20_round_trips() {
		for filter in [
			Filter::Unfiltered,
			Filter::NextObject,
			Filter::Relative(0),
			Filter::Relative(5),
			Filter::Absolute {
				start: Location { group: 12, object: 0 },
				end: None,
			},
			Filter::Absolute {
				start: Location { group: 12, object: 4 },
				end: Some(EndLocation {
					group: 20,
					object: None,
				}),
			},
		] {
			assert_eq!(round_trip(filter, NEW), filter, "{filter:?}");
		}
	}

	/// Absolute {0, 0} open ended is defined as equivalent to unfiltered, so it must not
	/// collide with the two-zero-field spelling of NextObject.
	#[test]
	fn draft20_absolute_origin_is_unfiltered() {
		let origin = Filter::Absolute {
			start: Location::default(),
			end: None,
		};
		assert!(value(origin, NEW).is_empty());
		assert_eq!(round_trip(origin, NEW), Filter::Unfiltered);
		assert_ne!(value(origin, NEW), value(Filter::NextObject, NEW));
	}

	#[test]
	fn draft19_uses_tags() {
		assert_eq!(value(Filter::NextObject, OLD), vec![tag::LARGEST_OBJECT as u8]);
		assert_eq!(value(Filter::Relative(0), OLD), vec![tag::NEXT_GROUP as u8]);
		for filter in [
			Filter::NextObject,
			Filter::Relative(0),
			Filter::Absolute {
				start: Location { group: 12, object: 4 },
				end: None,
			},
			Filter::Absolute {
				start: Location { group: 12, object: 4 },
				end: Some(EndLocation {
					group: 20,
					object: None,
				}),
			},
		] {
			assert_eq!(round_trip(filter, OLD), filter, "{filter:?}");
		}
	}

	/// The fourth field bounds the last object in the end group. Dropping it on decode
	/// would let a publisher deliver past the Location the subscriber asked for.
	#[test]
	fn draft20_keeps_the_end_object() {
		let bounded = Filter::Absolute {
			start: Location { group: 7, object: 3 },
			end: Some(EndLocation {
				group: 9,
				object: Some(4),
			}),
		};
		assert_eq!(value(bounded, NEW), vec![0x07, 0x03, 0x02, 0x04]);
		assert_eq!(round_trip(bounded, NEW), bounded);

		// Three fields leave the end group whole, which is a different filter.
		let whole = Filter::Absolute {
			start: Location { group: 7, object: 3 },
			end: Some(EndLocation { group: 9, object: None }),
		};
		assert_eq!(value(whole, NEW), vec![0x07, 0x03, 0x02]);
		assert_ne!(round_trip(bounded, NEW), whole);
	}

	/// Draft-19's AbsoluteRange ends on a group, so an object bound cannot be expressed.
	/// Widening the range silently would deliver objects the subscriber excluded.
	#[test]
	fn draft19_cannot_bound_the_end_object() {
		let mut buf = Vec::new();
		let bounded = Filter::Absolute {
			start: Location { group: 7, object: 0 },
			end: Some(EndLocation {
				group: 9,
				object: Some(4),
			}),
		};
		assert!(bounded.param_encode(&mut buf, OLD).is_err());
	}

	/// Draft-19 has a tag per case and none of them mean "two groups back", so refuse
	/// rather than silently sending a nearby filter the peer would honor.
	#[test]
	fn draft19_cannot_name_a_relative_group() {
		let mut buf = Vec::new();
		assert!(Filter::Relative(2).param_encode(&mut buf, OLD).is_err());
	}

	#[test]
	fn rejects_a_backwards_range() {
		let backwards = Filter::Absolute {
			start: Location { group: 9, object: 0 },
			end: Some(EndLocation { group: 4, object: None }),
		};
		for version in [OLD, NEW] {
			let mut buf = Vec::new();
			assert!(backwards.param_encode(&mut buf, version).is_err(), "{version}");
		}
	}

	#[test]
	fn rejects_too_many_fields() {
		let mut buf = Vec::new();
		vec![0u8; 5].encode(&mut buf, NEW).expect("encode");
		let mut bytes = bytes::Bytes::from(buf);
		assert!(Filter::param_decode(&mut bytes, NEW).is_err());
	}
}

/// How a parameter inside a FILL_PARAMETERS scope frames its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Framing {
	/// A single raw byte, which is what the draft's `uint8` parameters are.
	Byte,
	/// A variable-length integer.
	Varint,
	/// A length prefix followed by that many bytes.
	Bytes,
}

/// The FILL_PARAMETERS parameter (0x23), draft-20's request for a backfill.
///
/// Its presence is what asks for one. The value is a nested parameter scope describing the
/// fill, of which the Location Filter is the part we act on.
///
/// The publisher serves a fill whose range resolves to a single group, straight from the
/// model's group cache on a fetch stream. That covers the draft's own current-group join
/// (a Next Object subscription plus a `StartGroup=1` fill). A range spanning several
/// groups is refused by resetting the fetch stream, the draft's fill-failure signal,
/// because multi-group fetch serialization depends on a negotiated group order we do not
/// implement. We never request a fill ourselves; see `subscriber::subscribe_filter`.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fill {
	/// The range to fill. `None` means the Location Filter was omitted, which inherits the
	/// subscription's own filter; [`Filter::Unfiltered`] (a zero-length filter) means the
	/// whole track up to Largest Object.
	pub filter: Option<Filter>,

	/// Whether the scope carried a Range Filter (0x25-0x28). Those narrow which objects
	/// pass, which we do not implement, and serving the unfiltered range instead would
	/// deliver objects the peer excluded, so such a fill is refused rather than widened.
	/// Never encoded; we send no range filters.
	pub range_filters: bool,
}

impl Fill {
	/// LOCATION_FILTER, the only nested parameter that changes what we deliver.
	const LOCATION_FILTER: u64 = 0x21;

	/// The parameters the draft allows inside FILL_PARAMETERS, and how each frames its
	/// value. Anything absent from this table is a protocol violation rather than something
	/// to skip.
	///
	/// The framing is tabulated rather than derived, because neither shortcut is right. The
	/// Key-Value-Pair rule keys it off the id's parity, but the Range Filters (0x25-0x28)
	/// carry an explicit `Length` despite two of them having even ids. And a `uint8`
	/// parameter is one raw byte rather than a varint, so reading it as one misparses any
	/// value with a leading 1-bit. Either mistake desyncs every parameter after it.
	const ALLOWED: &'static [(u64, Framing)] = &[
		(0x0A, Framing::Varint), // FILL_TIMEOUT
		(0x20, Framing::Byte),   // SUBSCRIBER_PRIORITY, a uint8
		(Self::LOCATION_FILTER, Framing::Bytes),
		(0x22, Framing::Byte),  // GROUP_ORDER, a uint8
		(0x25, Framing::Bytes), // SUBGROUP_FILTER
		(0x26, Framing::Bytes), // OBJECTID_FILTER, length prefixed despite an even id
		(0x27, Framing::Bytes), // PRIORITY_FILTER
		(0x28, Framing::Bytes), // OBJECT_PROPERTY_FILTER, likewise
	];
}

impl Param for Fill {
	fn param_encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		let mut buf = Vec::new();

		// A nested scope is encoded like a message's parameters: a count, then the KVPs.
		// An omitted filter inherits the subscription's, so the scope is empty. An explicit
		// Unfiltered still encodes, as a zero-length filter meaning the whole track.
		match self.filter {
			None => 0u64.encode(&mut buf, version)?,
			Some(filter) => {
				1u64.encode(&mut buf, version)?;
				// The first type in a scope is not delta encoded, so this is the raw id.
				Self::LOCATION_FILTER.encode(&mut buf, version)?;
				filter.param_encode(&mut buf, version)?;
			}
		}

		buf.encode(w, version)
	}

	fn param_decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let data = Vec::<u8>::decode(r, version)?;
		let mut buf = bytes::Bytes::from(data);

		let count = u64::decode(&mut buf, version)?;
		if count > 64 {
			return Err(DecodeError::TooMany);
		}

		let mut filter = None;
		let mut range_filters = false;
		let mut prev = 0u64;
		for i in 0..count {
			let delta = u64::decode(&mut buf, version)?;
			let key = if i == 0 {
				delta
			} else {
				prev.checked_add(delta).ok_or(DecodeError::BoundsExceeded)?
			};
			prev = key;

			let Some((_, framing)) = Self::ALLOWED.iter().find(|(id, _)| *id == key) else {
				return Err(DecodeError::InvalidValue);
			};

			if key == Self::LOCATION_FILTER {
				if filter.is_some() {
					return Err(DecodeError::Duplicate);
				}
				filter = Some(Filter::param_decode(&mut buf, version)?);
				continue;
			}

			// A Range Filter changes which objects the fill may contain, so its presence
			// is recorded even though its value is not interpreted.
			range_filters |= (0x25..=0x28).contains(&key);

			// The rest are parameters we do not act on, but their bytes still have to be
			// consumed or the remaining keys desync.
			match framing {
				Framing::Bytes => {
					Vec::<u8>::decode(&mut buf, version)?;
				}
				Framing::Varint => {
					u64::decode(&mut buf, version)?;
				}
				Framing::Byte => {
					u8::decode(&mut buf, version)?;
				}
			}
		}

		if buf.has_remaining() {
			return Err(DecodeError::TrailingBytes);
		}

		Ok(Self { filter, range_filters })
	}
}

#[cfg(test)]
mod fill_tests {
	use super::*;

	const NEW: Version = Version::Draft20;

	fn round_trip(fill: Fill) -> Fill {
		let mut buf = Vec::new();
		fill.param_encode(&mut buf, NEW).expect("encode");
		let mut bytes = bytes::Bytes::from(buf);
		let decoded = Fill::param_decode(&mut bytes, NEW).expect("decode");
		assert!(!bytes.has_remaining());
		decoded
	}

	#[test]
	fn round_trips() {
		for filter in [
			// An omitted filter (inherit the subscription's) and an explicit zero-length
			// one (the whole track) are distinct spellings and must stay distinct.
			None,
			Some(Filter::Unfiltered),
			Some(Filter::Relative(1)),
			Some(Filter::Relative(3)),
			Some(Filter::Absolute {
				start: Location { group: 4, object: 0 },
				end: Some(EndLocation { group: 9, object: None }),
			}),
		] {
			let fill = Fill {
				filter,
				range_filters: false,
			};
			assert_eq!(round_trip(fill).filter, filter, "{filter:?}");
		}
	}

	/// The canonical current-group join: an empty subscription filter paired with a fill
	/// that starts one group back.
	#[test]
	fn current_group_join() {
		let fill = Fill {
			filter: Some(Filter::Relative(1)),
			range_filters: false,
		};
		let mut buf = Vec::new();
		fill.param_encode(&mut buf, NEW).expect("encode");
		// count=1, type=0x21, len=1, StartGroup=1
		let mut bytes = bytes::Bytes::from(buf);
		let value = Vec::<u8>::decode(&mut bytes, NEW).expect("length prefix");
		assert_eq!(value, vec![0x01, 0x21, 0x01, 0x01]);
	}

	/// A parameter the draft does not allow inside the scope is a violation, not something
	/// to skip past.
	#[test]
	fn rejects_a_disallowed_parameter() {
		let mut value = Vec::new();
		1u64.encode(&mut value, NEW).unwrap();
		0x10u64.encode(&mut value, NEW).unwrap(); // FORWARD, not allowed in a fill
		0u64.encode(&mut value, NEW).unwrap();

		let mut buf = Vec::new();
		value.encode(&mut buf, NEW).unwrap();
		let mut bytes = bytes::Bytes::from(buf);
		assert!(Fill::param_decode(&mut bytes, NEW).is_err());
	}

	/// A uint8 parameter is one raw byte, so a priority of 128 or more would be read as a
	/// multi-byte varint prefix and swallow the parameter after it.
	#[test]
	fn skips_a_uint8_whose_value_has_a_leading_one() {
		let mut value = Vec::new();
		2u64.encode(&mut value, NEW).unwrap();
		0x20u64.encode(&mut value, NEW).unwrap(); // SUBSCRIBER_PRIORITY
		0x80u8.encode(&mut value, NEW).unwrap(); // a raw byte, not a varint
		1u64.encode(&mut value, NEW).unwrap(); // delta to 0x21
		Filter::Relative(1).param_encode(&mut value, NEW).unwrap();

		let mut buf = Vec::new();
		value.encode(&mut buf, NEW).unwrap();
		let mut bytes = bytes::Bytes::from(buf);
		let fill = Fill::param_decode(&mut bytes, NEW).expect("decode");
		assert_eq!(fill.filter, Some(Filter::Relative(1)));
	}

	/// An allowed parameter we ignore still has to be consumed, or the keys after it
	/// desync. Its own definition decides how many bytes that is.
	/// OBJECTID_FILTER has an even id but is written with an explicit Length, so parity
	/// would read its length byte as the whole value and desync everything after it.
	#[test]
	fn skips_a_length_prefixed_range_filter() {
		let mut value = Vec::new();
		2u64.encode(&mut value, NEW).unwrap();
		0x26u64.encode(&mut value, NEW).unwrap(); // OBJECTID_FILTER, length prefixed
		vec![0xAAu8, 0xBB, 0xCC].encode(&mut value, NEW).unwrap();
		1u64.encode(&mut value, NEW).unwrap(); // delta to 0x27
		vec![0xDDu8].encode(&mut value, NEW).unwrap(); // PRIORITY_FILTER

		let mut buf = Vec::new();
		value.encode(&mut buf, NEW).unwrap();
		let mut bytes = bytes::Bytes::from(buf);
		let fill = Fill::param_decode(&mut bytes, NEW).expect("decode");
		assert_eq!(fill.filter, None, "no Location Filter in the scope means inherit");
		assert!(fill.range_filters, "a Range Filter's presence must be recorded");
	}

	#[test]
	fn skips_allowed_parameters_it_ignores() {
		let mut value = Vec::new();
		2u64.encode(&mut value, NEW).unwrap();
		0x20u64.encode(&mut value, NEW).unwrap(); // SUBSCRIBER_PRIORITY, even: one varint
		42u64.encode(&mut value, NEW).unwrap();
		1u64.encode(&mut value, NEW).unwrap(); // delta to 0x21, odd: length prefixed
		Filter::Relative(2).param_encode(&mut value, NEW).unwrap();

		let mut buf = Vec::new();
		value.encode(&mut buf, NEW).unwrap();
		let mut bytes = bytes::Bytes::from(buf);
		let fill = Fill::param_decode(&mut bytes, NEW).expect("decode");
		assert_eq!(fill.filter, Some(Filter::Relative(2)));
	}
}
