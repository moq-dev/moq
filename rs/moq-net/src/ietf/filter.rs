//! The Location Filter carried by SUBSCRIBE, PUBLISH and REQUEST_UPDATE.

use crate::coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder};

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
	/// Only draft-22 can encode a value above 1; older drafts have a tag per case.
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

/// The tagged Filter Type of draft-18 and earlier.
mod tag {
	pub const NEXT_GROUP: u64 = 0x1;
	pub const LARGEST_OBJECT: u64 = 0x2;
	pub const ABSOLUTE_START: u64 = 0x3;
	pub const ABSOLUTE_RANGE: u64 = 0x4;
}

/// The Location Filter Type of draft-22, which names the fields that follow it.
mod filter_type {
	pub const NONE: u64 = 0x0;
	pub const RELATIVE_START: u64 = 0x1;
	pub const ABSOLUTE_START: u64 = 0x2;
	pub const ABSOLUTE_GROUP_END: u64 = 0x3;
	pub const ABSOLUTE_RANGE: u64 = 0x4;
	pub const NEXT_OBJECT: u64 = 0x5;
}

impl Filter {
	/// Whether this is draft-20 or newer.
	///
	/// Draft-20 replaced the Filter Type tag with up to four optional varints, where the
	/// number present selects the meaning, and added the parameters that ride alongside it.
	/// Draft-22 names the meaning with a Location Filter Type instead, but keeps those
	/// parameters, so this still gates them.
	pub(crate) fn is_draft20(version: Version) -> bool {
		!matches!(version, Version::Draft18)
	}

	/// The absolute end group, given the start, or an error when the range runs backwards.
	fn end_delta(start: u64, end: u64) -> Result<u64, EncodeError> {
		end.checked_sub(start).ok_or(EncodeError::InvalidState)
	}

	/// Encode the draft-20 field list, without the enclosing length prefix.
	fn encode_fields(&self, w: &mut Encoder<'_>) -> Result<(), EncodeError> {
		match *self {
			// Zero fields. The draft defines an open ended absolute {0, 0} as equivalent, so
			// that spelling normalizes to this one rather than colliding with NextObject.
			Self::Unfiltered => {}
			Self::NextObject => {
				w.varint(0)?;
				w.varint(0)?;
			}
			Self::Relative(groups) => w.varint(groups)?,
			Self::Absolute {
				start: Location { group: 0, object: 0 },
				end: None,
			} => {}
			Self::Absolute { start, end } => Self::encode_absolute(w, start, end)?,
		}
		Ok(())
	}

	/// Encode the start, then as much of the end as is set.
	fn encode_absolute(w: &mut Encoder<'_>, start: Location, end: Option<EndLocation>) -> Result<(), EncodeError> {
		w.varint(start.group)?;
		w.varint(start.object)?;
		if let Some(end) = end {
			w.varint(Self::end_delta(start.group, end.group)?)?;
			if let Some(object) = end.object {
				w.varint(object)?;
			}
		}
		Ok(())
	}

	/// Encode the draft-22 form: the Location Filter Type, then only the fields it names.
	fn encode_typed(&self, w: &mut Encoder<'_>) -> Result<(), EncodeError> {
		match *self {
			Self::Unfiltered => w.varint(filter_type::NONE)?,
			Self::NextObject => w.varint(filter_type::NEXT_OBJECT)?,
			Self::Relative(groups) => {
				w.varint(filter_type::RELATIVE_START)?;
				w.varint(groups)?;
			}
			Self::Absolute { start, end } => {
				w.varint(match end {
					None => filter_type::ABSOLUTE_START,
					Some(EndLocation { object: None, .. }) => filter_type::ABSOLUTE_GROUP_END,
					Some(EndLocation { object: Some(_), .. }) => filter_type::ABSOLUTE_RANGE,
				})?;
				Self::encode_absolute(w, start, end)?;
			}
		}
		Ok(())
	}

	/// Decode the draft-22 form, reading only the fields its Location Filter Type names.
	fn decode_typed(r: &mut Decoder<'_>) -> Result<Self, DecodeError> {
		Ok(match r.varint()? {
			filter_type::NONE => Self::Unfiltered,
			filter_type::RELATIVE_START => Self::Relative(r.varint()?),
			kind @ (filter_type::ABSOLUTE_START | filter_type::ABSOLUTE_GROUP_END | filter_type::ABSOLUTE_RANGE) => {
				let start = Location {
					group: r.varint()?,
					object: r.varint()?,
				};
				let end = match kind {
					filter_type::ABSOLUTE_START => None,
					_ => Some(EndLocation {
						group: start
							.group
							.checked_add(r.varint()?)
							.ok_or(DecodeError::BoundsExceeded)?,
						object: match kind {
							filter_type::ABSOLUTE_RANGE => Some(r.varint()?),
							_ => None,
						},
					}),
				};
				Self::Absolute { start, end }
			}
			filter_type::NEXT_OBJECT => Self::NextObject,
			_ => return Err(DecodeError::InvalidValue),
		})
	}

	/// Decode the draft-20 field list, which the caller has already delimited.
	fn decode_fields(r: &mut Decoder<'_>) -> Result<Self, DecodeError> {
		let mut fields = Vec::with_capacity(4);
		while !r.is_empty() {
			if fields.len() == 4 {
				return Err(DecodeError::TrailingBytes);
			}
			fields.push(r.varint()?);
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

	/// Encode the draft-18 and earlier tag form, without the enclosing length prefix.
	fn encode_tag(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		match *self {
			// No tag means "everything", which only the absolute spelling can say.
			Self::Unfiltered => {
				w.varint(tag::ABSOLUTE_START)?;
				Location::default().encode(w, version)?;
			}
			Self::NextObject => w.varint(tag::LARGEST_OBJECT)?,
			Self::Relative(0) => w.varint(tag::NEXT_GROUP)?,
			// Only draft-22 can name a start further back than the next group without
			// knowing Largest Object, so there is no honest tag to fall back to.
			Self::Relative(_) => return Err(EncodeError::Unsupported),
			Self::Absolute { start, end: None } => {
				w.varint(tag::ABSOLUTE_START)?;
				start.encode(w, version)?;
			}
			// Draft-18's AbsoluteRange ends on a group, so an object-bounded range has no
			// spelling. Refuse rather than widen the range the caller asked for.
			Self::Absolute {
				end: Some(EndLocation { object: Some(_), .. }),
				..
			} => return Err(EncodeError::Unsupported),
			Self::Absolute { start, end: Some(end) } => {
				w.varint(tag::ABSOLUTE_RANGE)?;
				start.encode(w, version)?;
				w.varint(Self::end_delta(start.group, end.group)?)?;
			}
		}
		Ok(())
	}

	/// Decode the draft-18 and earlier tag form.
	fn decode_tag(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		Ok(match r.varint()? {
			tag::NEXT_GROUP => Self::Relative(0),
			tag::LARGEST_OBJECT => Self::NextObject,
			tag::ABSOLUTE_START => Self::Absolute {
				start: Location::decode(r, version)?,
				end: None,
			},
			tag::ABSOLUTE_RANGE => {
				let start = Location::decode(r, version)?;
				let delta = r.varint()?;
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

impl Param for Filter {
	/// An unfiltered subscription is the default, so it goes on the wire as an absent
	/// parameter rather than an empty one.
	fn param_present(&self) -> bool {
		!matches!(self, Self::Unfiltered)
	}

	fn param_encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		let mut buf = Vec::new();
		let mut inner = Encoder::new(&mut buf, version.into());
		match version {
			Version::Draft18 => self.encode_tag(&mut inner, version)?,
			Version::Draft20 => self.encode_fields(&mut inner)?,
			// Draft-22 drops the Length, since the Location Filter Type says which fields follow.
			_ => return self.encode_typed(w),
		}

		w.bytes(&buf)
	}

	fn param_decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Draft18 => {}
			// The field count is what carries the meaning, so the value is consumed whole.
			Version::Draft20 => return Self::decode_fields(&mut Decoder::new(r.bytes()?, version.into())),
			_ => return Self::decode_typed(r),
		}

		let mut buf = Decoder::new(r.bytes()?, version.into());
		let filter = Self::decode_tag(&mut buf, version)?;
		if !buf.is_empty() {
			return Err(DecodeError::TrailingBytes);
		}
		Ok(filter)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const OLD: Version = Version::Draft18;
	const FIELDS: Version = Version::Draft20;
	const TYPED: Version = Version::Draft22;

	fn round_trip(filter: Filter, version: Version) -> Filter {
		let mut buf = Vec::new();
		filter
			.param_encode(&mut Encoder::new(&mut buf, version.into()), version)
			.expect("encode");
		let mut bytes = bytes::Bytes::from(buf);
		let decoded = crate::coding::decode_buf(&mut bytes, version, Filter::param_decode).expect("decode");
		assert!(bytes.is_empty(), "parameter left trailing bytes");
		decoded
	}

	/// The value bytes, without the length prefix a parameter carries.
	fn value(filter: Filter, version: Version) -> Vec<u8> {
		let mut buf = Vec::new();
		filter
			.param_encode(&mut Encoder::new(&mut buf, version.into()), version)
			.expect("encode");
		let mut bytes = bytes::Bytes::from(buf);
		crate::coding::decode_buf(&mut bytes, version, |r, _| Ok(r.bytes()?.to_vec())).expect("length prefix")
	}

	#[test]
	fn draft20_field_counts() {
		// The number of fields present is what selects the meaning, so pin the bytes.
		assert_eq!(value(Filter::NextObject, FIELDS), vec![0x00, 0x00]);
		assert_eq!(value(Filter::Relative(0), FIELDS), vec![0x00]);
		assert_eq!(value(Filter::Relative(1), FIELDS), vec![0x01]);
		assert_eq!(
			value(
				Filter::Absolute {
					start: Location { group: 7, object: 3 },
					end: None
				},
				FIELDS
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
				FIELDS
			),
			vec![0x07, 0x03, 0x02]
		);
	}

	/// Draft-17 replaced QUIC's varint with a leading-1-bits one. They agree below 64, so a
	/// wrong codec stays invisible until group or object ids grow past that.
	#[test]
	fn draft20_uses_leading_ones_varints_above_63() {
		// 128 is 0x80_80 with leading ones, and 0x40_80 with the QUIC form.
		assert_eq!(value(Filter::Relative(128), FIELDS), vec![0x80, 0x80]);
		assert_eq!(round_trip(Filter::Relative(128), FIELDS), Filter::Relative(128));

		let wide = Filter::Absolute {
			start: Location { group: 300, object: 64 },
			end: None,
		};
		assert_eq!(round_trip(wide, FIELDS), wide);
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
			assert_eq!(round_trip(filter, FIELDS), filter, "{filter:?}");
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
		assert!(value(origin, FIELDS).is_empty());
		assert_eq!(round_trip(origin, FIELDS), Filter::Unfiltered);
		assert_ne!(value(origin, FIELDS), value(Filter::NextObject, FIELDS));
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
		assert_eq!(value(bounded, FIELDS), vec![0x07, 0x03, 0x02, 0x04]);
		assert_eq!(round_trip(bounded, FIELDS), bounded);

		// Three fields leave the end group whole, which is a different filter.
		let whole = Filter::Absolute {
			start: Location { group: 7, object: 3 },
			end: Some(EndLocation { group: 9, object: None }),
		};
		assert_eq!(value(whole, FIELDS), vec![0x07, 0x03, 0x02]);
		assert_ne!(round_trip(bounded, FIELDS), whole);
	}

	#[test]
	fn draft20_rejects_too_many_fields() {
		let mut buf = Vec::new();
		Encoder::new(&mut buf, FIELDS.into()).bytes(&[0u8; 5]).unwrap();
		let mut bytes = bytes::Bytes::from(buf);
		assert!(crate::coding::decode_buf(&mut bytes, FIELDS, Filter::param_decode).is_err());
	}

	#[test]
	fn draft18_uses_tags() {
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

	/// Draft-18's AbsoluteRange ends on a group, so an object bound cannot be expressed.
	/// Widening the range silently would deliver objects the subscriber excluded.
	#[test]
	fn draft18_cannot_bound_the_end_object() {
		let mut buf = Vec::new();
		let bounded = Filter::Absolute {
			start: Location { group: 7, object: 0 },
			end: Some(EndLocation {
				group: 9,
				object: Some(4),
			}),
		};
		assert!(
			bounded
				.param_encode(&mut Encoder::new(&mut buf, OLD.into()), OLD)
				.is_err()
		);
	}

	/// Draft-18 has a tag per case and none of them mean "two groups back", so refuse
	/// rather than silently sending a nearby filter the peer would honor.
	#[test]
	fn draft18_cannot_name_a_relative_group() {
		let mut buf = Vec::new();
		assert!(
			Filter::Relative(2)
				.param_encode(&mut Encoder::new(&mut buf, OLD.into()), OLD)
				.is_err()
		);
	}

	#[test]
	fn rejects_a_backwards_range() {
		let backwards = Filter::Absolute {
			start: Location { group: 9, object: 0 },
			end: Some(EndLocation { group: 4, object: None }),
		};
		for version in [OLD, FIELDS, TYPED] {
			let mut buf = Vec::new();
			assert!(
				backwards
					.param_encode(&mut Encoder::new(&mut buf, version.into()), version)
					.is_err(),
				"{version}"
			);
		}
	}

	/// The whole parameter value as it sits on the wire, which from draft-22 has no Length.
	fn raw(filter: Filter, version: Version) -> Vec<u8> {
		let mut buf = Vec::new();
		filter
			.param_encode(&mut Encoder::new(&mut buf, version.into()), version)
			.expect("encode");
		buf
	}

	fn decode_raw(bytes: &[u8], version: Version) -> Result<(Filter, usize), DecodeError> {
		let mut r = Decoder::new(bytes, version.into());
		let filter = Filter::param_decode(&mut r, version)?;
		Ok((filter, r.remaining()))
	}

	/// Draft-22 section 9.20.9: a Location Filter Type, then only the fields it names.
	#[test]
	fn draft22_carries_the_type() {
		let start = Location { group: 7, object: 3 };
		for (filter, bytes) in [
			(Filter::Unfiltered, vec![0x00]),
			(Filter::Relative(1), vec![0x01, 0x01]),
			(Filter::Absolute { start, end: None }, vec![0x02, 0x07, 0x03]),
			(
				Filter::Absolute {
					start,
					end: Some(EndLocation { group: 9, object: None }),
				},
				vec![0x03, 0x07, 0x03, 0x02],
			),
			(
				Filter::Absolute {
					start,
					end: Some(EndLocation {
						group: 9,
						object: Some(4),
					}),
				},
				vec![0x04, 0x07, 0x03, 0x02, 0x04],
			),
			(Filter::NextObject, vec![0x05]),
			// The type says absolute, so {0, 0} no longer has to borrow the unfiltered spelling.
			(
				Filter::Absolute {
					start: Location::default(),
					end: None,
				},
				vec![0x02, 0x00, 0x00],
			),
		] {
			assert_eq!(raw(filter, TYPED), bytes, "{filter:?}");
			assert_eq!(decode_raw(&bytes, TYPED).unwrap(), (filter, 0), "{filter:?}");
		}
	}

	/// With no Length, the type alone says where the value ends, so the parameter after it
	/// must be left untouched.
	#[test]
	fn draft22_reads_only_the_named_fields() {
		assert_eq!(decode_raw(&[0x05, 0x22, 0x01], TYPED).unwrap(), (Filter::NextObject, 2));
		assert_eq!(
			decode_raw(&[0x01, 0x01, 0x22, 0x01], TYPED).unwrap(),
			(Filter::Relative(1), 2)
		);
	}

	/// Draft-17 replaced QUIC's varint with a leading-1-bits one. They agree below 64, so a
	/// wrong codec stays invisible until group or object ids grow past that.
	#[test]
	fn draft22_uses_leading_ones_varints_above_63() {
		// 128 is 0x80_80 with leading ones, and 0x40_80 with the QUIC form.
		assert_eq!(raw(Filter::Relative(128), TYPED), vec![0x01, 0x80, 0x80]);
		assert_eq!(
			decode_raw(&[0x01, 0x80, 0x80], TYPED).unwrap(),
			(Filter::Relative(128), 0)
		);
	}

	#[test]
	fn draft22_rejects_an_unknown_type() {
		for kind in [0x06u8, 0x3f] {
			assert!(matches!(
				decode_raw(&[kind, 0x00, 0x00], TYPED),
				Err(DecodeError::InvalidValue)
			));
		}
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

/// The FILL_PARAMETERS parameter (0x23), draft-22's request for a backfill.
///
/// Its presence is what asks for one. The value is a nested parameter scope describing the
/// fill's Location Filter and group order.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fill {
	/// The order of the filled groups, or the subscription's order when omitted.
	pub group_order: Option<super::GroupOrder>,
	/// The range to fill. `None` means the Location Filter was omitted, which inherits the
	/// subscription's own filter; [`Filter::Unfiltered`] (a zero-length filter, or type 0x00
	/// from draft-22) means the whole track up to Largest Object.
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

	/// The parameters the draft allows inside FILL_PARAMETERS besides LOCATION_FILTER, and
	/// how each frames its value. Anything else is a protocol violation rather than
	/// something to skip.
	///
	/// The framing is tabulated rather than derived, because neither shortcut is right. The
	/// Key-Value-Pair rule keys it off the id's parity, but the Range Filters (0x25-0x28)
	/// carry an explicit `Length` despite two of them having even ids. And a `uint8`
	/// parameter is one raw byte rather than a varint, so reading it as one misparses any
	/// value with a leading 1-bit. Either mistake desyncs every parameter after it.
	const ALLOWED: &'static [(u64, Framing)] = &[
		(0x0A, Framing::Varint), // FILL_TIMEOUT
		(0x20, Framing::Byte),   // SUBSCRIBER_PRIORITY, a uint8
		(0x22, Framing::Byte),   // GROUP_ORDER, a uint8
		(0x25, Framing::Bytes),  // SUBGROUP_FILTER
		(0x26, Framing::Bytes),  // OBJECTID_FILTER, length prefixed despite an even id
		(0x27, Framing::Bytes),  // PRIORITY_FILTER
		(0x28, Framing::Bytes),  // OBJECT_PROPERTY_FILTER, likewise
	];
}

impl Param for Fill {
	fn param_encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if self.group_order == Some(super::GroupOrder::Any) {
			return Err(EncodeError::Unsupported);
		}
		let mut buf = Vec::new();
		let mut inner = Encoder::new(&mut buf, w.form());

		// Parameters sort by id; the later type is encoded as a delta.
		inner.varint(u64::from(self.filter.is_some()) + u64::from(self.group_order.is_some()))?;
		if let Some(filter) = self.filter {
			inner.varint(Self::LOCATION_FILTER)?;
			filter.param_encode(&mut inner, version)?;
		}
		if let Some(order) = self.group_order {
			inner.varint(if self.filter.is_some() { 1 } else { 0x22 })?;
			order.param_encode(&mut inner, version)?;
		}

		w.bytes(&buf)
	}

	fn param_decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let mut buf = Decoder::new(r.bytes()?, r.form());

		let count = buf.varint()?;
		if count > 64 {
			return Err(DecodeError::TooMany);
		}

		let mut filter = None;
		let mut group_order = None;
		let mut range_filters = false;
		let mut prev = 0u64;
		for i in 0..count {
			let delta = buf.varint()?;
			let key = if i == 0 {
				delta
			} else {
				prev.checked_add(delta).ok_or(DecodeError::BoundsExceeded)?
			};
			prev = key;

			// Its framing depends on the draft, so the filter reads itself.
			if key == Self::LOCATION_FILTER {
				if filter.is_some() {
					return Err(DecodeError::Duplicate);
				}
				filter = Some(Filter::param_decode(&mut buf, version)?);
				continue;
			}

			let Some((_, framing)) = Self::ALLOWED.iter().find(|(id, _)| *id == key) else {
				return Err(DecodeError::InvalidValue);
			};

			// A Range Filter changes which objects the fill may contain, so its presence
			// is recorded even though its value is not interpreted.
			range_filters |= (0x25..=0x28).contains(&key);

			// Parameters whose values are not retained still have to be consumed.
			match framing {
				Framing::Bytes => {
					buf.bytes()?;
				}
				Framing::Varint => {
					buf.varint()?;
				}
				Framing::Byte => {
					if key == 0x22 {
						if group_order
							.replace(super::GroupOrder::param_decode(&mut buf, version)?)
							.is_some()
						{
							return Err(DecodeError::Duplicate);
						}
					} else {
						buf.u8()?;
					}
				}
			}
		}

		if !buf.is_empty() {
			return Err(DecodeError::TrailingBytes);
		}

		Ok(Self {
			filter,
			group_order,
			range_filters,
		})
	}
}

#[cfg(test)]
mod fill_tests {
	use super::*;

	const NEW: Version = Version::Draft22;

	#[test]
	fn fill_rejects_invalid_group_order() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let fill = Fill {
				group_order: Some(super::super::GroupOrder::Any),
				filter: None,
				range_filters: false,
			};
			let mut bytes = Vec::new();
			assert!(matches!(
				fill.param_encode(&mut Encoder::new(&mut bytes, version.into()), version),
				Err(EncodeError::Unsupported)
			));
		}
		let version = Version::Draft22;
		for value in [0, 3, 255] {
			let bytes = [3, 1, 0x22, value];
			let mut r = Decoder::new(&bytes, version.into());
			assert!(matches!(
				Fill::param_decode(&mut r, version),
				Err(DecodeError::InvalidValue)
			));
		}
		for value in [1, 2] {
			let bytes = [3, 1, 0x22, value];
			let mut r = Decoder::new(&bytes, version.into());
			assert_eq!(
				Fill::param_decode(&mut r, version).unwrap().group_order,
				Some(match value {
					1 => super::super::GroupOrder::Ascending,
					_ => super::super::GroupOrder::Descending,
				})
			);
		}
	}

	fn round_trip(fill: Fill, version: Version) -> Fill {
		let mut buf = Vec::new();
		fill.param_encode(&mut Encoder::new(&mut buf, version.into()), version)
			.expect("encode");
		let mut bytes = bytes::Bytes::from(buf);
		let decoded = crate::coding::decode_buf(&mut bytes, version, Fill::param_decode).expect("decode");
		assert!(bytes.is_empty());
		decoded
	}

	/// The scope's value, without the length prefix FILL_PARAMETERS carries.
	fn value(filter: Filter, version: Version) -> Vec<u8> {
		let fill = Fill {
			group_order: None,
			filter: Some(filter),
			range_filters: false,
		};
		let mut buf = Vec::new();
		fill.param_encode(&mut Encoder::new(&mut buf, version.into()), version)
			.expect("encode");
		let mut bytes = bytes::Bytes::from(buf);
		crate::coding::decode_buf(&mut bytes, version, |r, _| Ok(r.bytes()?.to_vec())).expect("length prefix")
	}

	#[test]
	fn round_trips() {
		for version in [Version::Draft20, NEW] {
			round_trips_on(version);
		}
	}

	fn round_trips_on(version: Version) {
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
			for group_order in [
				None,
				Some(super::super::GroupOrder::Ascending),
				Some(super::super::GroupOrder::Descending),
			] {
				let fill = Fill {
					filter,
					group_order,
					range_filters: false,
				};
				assert_eq!(round_trip(fill, version), fill, "{version} {filter:?} {group_order:?}");
			}
		}
	}

	/// The canonical current-group join: an empty subscription filter paired with a fill
	/// that starts one group back.
	#[test]
	fn current_group_join() {
		// count=1, type=0x21, Relative Start, StartGroup=1
		assert_eq!(value(Filter::Relative(1), NEW), vec![0x01, 0x21, 0x01, 0x01]);
	}

	/// Draft-20's nested LOCATION_FILTER is a Length and its field list.
	#[test]
	fn draft20_nested_filter_counts_its_fields() {
		let range = Filter::Absolute {
			start: Location { group: 128, object: 0 },
			end: Some(EndLocation {
				group: 130,
				object: None,
			}),
		};
		let version = Version::Draft20;
		assert_eq!(value(Filter::Relative(1), version), vec![0x01, 0x21, 0x01, 0x01]);
		assert_eq!(value(range, version), vec![0x01, 0x21, 0x04, 0x80, 0x80, 0x00, 0x02]);
		assert_eq!(value(Filter::Unfiltered, version), vec![0x01, 0x21, 0x00]);
	}

	/// The nested LOCATION_FILTER carries its type instead of a Length, and the
	/// whole-track form the spec calls zero-length is type 0x00.
	#[test]
	fn draft22_nested_filter_carries_its_type() {
		let range = Filter::Absolute {
			start: Location { group: 128, object: 0 },
			end: Some(EndLocation {
				group: 130,
				object: None,
			}),
		};
		assert_eq!(value(range, NEW), vec![0x01, 0x21, 0x03, 0x80, 0x80, 0x00, 0x02]);
		assert_eq!(value(Filter::NextObject, NEW), vec![0x01, 0x21, 0x05]);
		assert_eq!(value(Filter::Unfiltered, NEW), vec![0x01, 0x21, 0x00]);
	}

	/// A parameter the draft does not allow inside the scope is a violation, not something
	/// to skip past.
	#[test]
	fn rejects_a_disallowed_parameter() {
		let mut value = Vec::new();
		Encoder::new(&mut value, NEW.into()).varint(1u64).unwrap();
		Encoder::new(&mut value, NEW.into()).varint(0x10u64).unwrap(); // FORWARD, not allowed in a fill
		Encoder::new(&mut value, NEW.into()).varint(0).unwrap();

		let mut buf = Vec::new();
		Encoder::new(&mut buf, NEW.into()).bytes(&value).unwrap();
		let mut bytes = bytes::Bytes::from(buf);
		assert!(crate::coding::decode_buf(&mut bytes, NEW, Fill::param_decode).is_err());
	}

	/// A uint8 parameter is one raw byte, so a priority of 128 or more would be read as a
	/// multi-byte varint prefix and swallow the parameter after it.
	#[test]
	fn skips_a_uint8_whose_value_has_a_leading_one() {
		let mut value = Vec::new();
		Encoder::new(&mut value, NEW.into()).varint(2u64).unwrap();
		Encoder::new(&mut value, NEW.into()).varint(0x20u64).unwrap(); // SUBSCRIBER_PRIORITY
		Encoder::new(&mut value, NEW.into()).u8(0x80u8); // a raw byte, not a varint
		Encoder::new(&mut value, NEW.into()).varint(1u64).unwrap(); // delta to 0x21
		Filter::Relative(1)
			.param_encode(&mut Encoder::new(&mut value, NEW.into()), NEW)
			.unwrap();

		let mut buf = Vec::new();
		Encoder::new(&mut buf, NEW.into()).bytes(&value).unwrap();
		let mut bytes = bytes::Bytes::from(buf);
		let fill = crate::coding::decode_buf(&mut bytes, NEW, Fill::param_decode).expect("decode");
		assert_eq!(fill.filter, Some(Filter::Relative(1)));
	}

	/// An allowed parameter we ignore still has to be consumed, or the keys after it
	/// desync. Its own definition decides how many bytes that is.
	/// OBJECTID_FILTER has an even id but is written with an explicit Length, so parity
	/// would read its length byte as the whole value and desync everything after it.
	#[test]
	fn skips_a_length_prefixed_range_filter() {
		let mut value = Vec::new();
		Encoder::new(&mut value, NEW.into()).varint(2u64).unwrap();
		Encoder::new(&mut value, NEW.into()).varint(0x26u64).unwrap(); // OBJECTID_FILTER, length prefixed
		Encoder::new(&mut value, NEW.into())
			.bytes(&[0xAAu8, 0xBB, 0xCC])
			.unwrap();
		Encoder::new(&mut value, NEW.into()).varint(1u64).unwrap(); // delta to 0x27
		Encoder::new(&mut value, NEW.into()).bytes(&[0xDDu8]).unwrap(); // PRIORITY_FILTER

		let mut buf = Vec::new();
		Encoder::new(&mut buf, NEW.into()).bytes(&value).unwrap();
		let mut bytes = bytes::Bytes::from(buf);
		let fill = crate::coding::decode_buf(&mut bytes, NEW, Fill::param_decode).expect("decode");
		assert_eq!(fill.filter, None, "no Location Filter in the scope means inherit");
		assert!(fill.range_filters, "a Range Filter's presence must be recorded");
	}

	#[test]
	fn skips_allowed_parameters_it_ignores() {
		let mut value = Vec::new();
		Encoder::new(&mut value, NEW.into()).varint(2u64).unwrap();
		Encoder::new(&mut value, NEW.into()).varint(0x20u64).unwrap(); // SUBSCRIBER_PRIORITY, even: one varint
		Encoder::new(&mut value, NEW.into()).varint(42u64).unwrap();
		Encoder::new(&mut value, NEW.into()).varint(1u64).unwrap(); // delta to 0x21
		Filter::Relative(2)
			.param_encode(&mut Encoder::new(&mut value, NEW.into()), NEW)
			.unwrap();

		let mut buf = Vec::new();
		Encoder::new(&mut buf, NEW.into()).bytes(&value).unwrap();
		let mut bytes = bytes::Bytes::from(buf);
		let fill = crate::coding::decode_buf(&mut bytes, NEW, Fill::param_decode).expect("decode");
		assert_eq!(fill.filter, Some(Filter::Relative(2)));
	}
}
