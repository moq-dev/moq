use std::borrow::Cow;

use crate::{
	Path,
	coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder},
};

use super::{Message, Version};

/// Sent by the subscriber to request all future objects for the given track.
///
/// Objects will use the provided ID instead of the full track name, to save bytes.
#[derive(Clone, Debug)]
pub struct Subscribe<'a> {
	pub id: u64,
	pub broadcast: Path<'a>,
	/// The publisher instance the subscriber expects; see [`crate::origin::Route::epoch`].
	/// Lite07+ only.
	pub epoch: Option<crate::Epoch>,
	pub track: Cow<'a, str>,
	pub priority: u8,
	pub max_delay: std::time::Duration,
	/// The minimum group to deliver (a floor). On lite-06 the wire carries the raw
	/// sequence and `None` is interchangeable with `Some(0)`: a floor of 0 constrains
	/// nothing, and the start resolves from `max_delay`. Pre-06 wires encode the
	/// sequence + 1 and an absent start means the latest group.
	pub start_group: Option<u64>,
	pub end_group: Option<u64>,
	/// First frame to deliver within `start_group`'s group; 0 is the whole group.
	/// Lite06+ only. It qualifies the named group, so it needs `start_group` to name one
	/// (`Some`, including `Some(0)`: group 0 can host a mid-group resume).
	pub start_frame: u64,
	/// Last frame to deliver (inclusive) within `end_group`'s group, or `None` for the
	/// whole group. Lite06+ only, and meaningless without an explicit `end_group`.
	pub end_frame: Option<u64>,
}

impl Version {
	/// Whether this version's SUBSCRIBE carries the subscriber's max delay preference.
	///
	/// Lite01/02 have no field for it, so a decoded `std::time::Duration::ZERO` there means
	/// "not stated", not "real time". Callers that act on the budget must tell the
	/// two apart or they will hold every legacy peer to the live edge.
	pub(crate) fn carries_max_delay(self) -> bool {
		!matches!(self, Version::Lite01 | Version::Lite02)
	}
}

impl Message for Subscribe<'_> {
	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let id = r.varint()?;
		let broadcast = Path::decode(r, version)?;
		let epoch = super::epoch::decode_epoch(r, version)?;
		let track = Cow::Owned(r.string()?);
		let priority = r.u8()?;

		let (max_delay, start_group, end_group) = match version {
			Version::Lite01 | Version::Lite02 => (std::time::Duration::ZERO, None, None),
			_ => {
				skip_group_order(r, version)?;
				let max_delay = std::time::Duration::from_millis(r.varint()?);
				let start_group = decode_start_group(r, version)?;
				let end_group = r.varint_opt()?;
				(max_delay, start_group, end_group)
			}
		};

		let (start_frame, end_frame) = decode_frame_bounds(r, version, start_group, end_group)?;
		let start_group = canonical_start_group(version, start_group, start_frame);

		Ok(Self {
			id,
			broadcast,
			epoch,
			track,
			priority,
			max_delay,
			start_group,
			end_group,
			start_frame,
			end_frame,
		})
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		w.varint(self.id)?;
		self.broadcast.encode(w, version)?;
		super::epoch::encode_epoch(w, version, self.epoch.as_ref())?;
		w.string(&self.track)?;
		w.u8(self.priority);

		match version {
			Version::Lite01 | Version::Lite02 => {}
			_ => {
				pad_group_order(w, version)?;
				w.varint(u64::try_from(self.max_delay.as_millis()).map_err(|_| EncodeError::BoundsExceeded)?)?;
				encode_start_group(w, version, self.start_group)?;
				w.varint_opt(self.end_group)?;
			}
		}

		encode_frame_bounds(
			w,
			version,
			self.start_group,
			self.start_frame,
			self.end_group,
			self.end_frame,
		)?;

		Ok(())
	}
}

/// Step over the retired `Ordered` byte on a version whose layout still has it.
///
/// The value is ignored: group order is fixed, so a peer that still sets it gets the
/// same newest-first delivery as one that doesn't.
pub(super) fn skip_group_order(r: &mut Decoder<'_>, version: Version) -> Result<(), DecodeError> {
	if version.has_group_order() {
		r.u8()?;
	}
	Ok(())
}

/// Write the retired `Ordered` byte as 0, keeping a deployed version's field offsets.
pub(super) fn pad_group_order(w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
	if version.has_group_order() {
		w.u8(0);
	}
	Ok(())
}

/// Decode the `Group Start` field shared by SUBSCRIBE and SUBSCRIBE_UPDATE.
///
/// Lite-06 carries the raw floor, so every value names a concrete group and 0 decodes as
/// `Some(0)`. Pre-06 wires encode the sequence + 1, with 0 meaning the latest group
/// (`None`). Callers canonicalize with [`canonical_start_group`] once the frame bounds
/// are known.
fn decode_start_group(r: &mut Decoder<'_>, version: Version) -> Result<Option<u64>, DecodeError> {
	if version.resolves_start() {
		return Ok(Some(r.varint()?));
	}
	r.varint_opt()
}

/// Canonicalize a decoded floor: a lite-06 `Group Start` of 0 with no frame offset is the
/// same absence of a constraint as no floor at all, so it decodes as `None`. Group 0 stays
/// named only when a `Frame Start` actually qualifies it (a subscription can resume
/// partway through group 0, e.g. a catalog that never leaves it).
fn canonical_start_group(version: Version, start_group: Option<u64>, start_frame: u64) -> Option<u64> {
	match (start_group, start_frame) {
		(Some(0), 0) if version.resolves_start() => None,
		(start_group, _) => start_group,
	}
}

/// Encode the `Group Start` field shared by SUBSCRIBE and SUBSCRIBE_UPDATE.
///
/// The inverse of [`decode_start_group`]: lite-06 writes the raw floor (`None` and
/// `Some(0)` are the same absence of a constraint), while a pre-06 wire gets `Some(0)`
/// folded back to absent. On those wires an explicit group 0 means "replay from the
/// beginning", which is not what a vacuous floor asks for.
fn encode_start_group(w: &mut Encoder<'_>, version: Version, start_group: Option<u64>) -> Result<(), EncodeError> {
	if version.resolves_start() {
		return w.varint(start_group.unwrap_or(0));
	}
	w.varint_opt(start_group.filter(|&group| group > 0))
}

/// Decode the trailing `Frame Start` / `Frame End` pair shared by SUBSCRIBE,
/// SUBSCRIBE_UPDATE, and FETCH.
///
/// Older versions carry no such fields, so they decode as the whole group. A frame bound
/// without the group bound it qualifies is a protocol violation: frames are numbered per
/// group, so there is nothing to count from.
fn decode_frame_bounds(
	r: &mut Decoder<'_>,
	version: Version,
	start_group: Option<u64>,
	end_group: Option<u64>,
) -> Result<(u64, Option<u64>), DecodeError> {
	if !version.has_frame_bounds() {
		return Ok((0, None));
	}

	let start_frame = r.varint()?;
	let end_frame = r.varint_opt()?;

	if (start_frame != 0 && start_group.is_none()) || (end_frame.is_some() && end_group.is_none()) {
		return Err(DecodeError::InvalidSubscribeLocation);
	}

	Ok((start_frame, end_frame))
}

/// Encode the trailing `Frame Start` / `Frame End` pair, a no-op before lite-06.
fn encode_frame_bounds(
	w: &mut Encoder<'_>,
	version: Version,
	start_group: Option<u64>,
	start_frame: u64,
	end_group: Option<u64>,
	end_frame: Option<u64>,
) -> Result<(), EncodeError> {
	if (start_frame != 0 && start_group.is_none()) || (end_frame.is_some() && end_group.is_none()) {
		return Err(EncodeError::InvalidState);
	}

	if !version.has_frame_bounds() {
		// Nothing carries the bounds, so silently widening to the whole group would
		// deliver frames the caller excluded. Refuse instead.
		if start_frame != 0 || end_frame.is_some() {
			return Err(EncodeError::Version);
		}
		return Ok(());
	}

	w.varint(start_frame)?;
	w.varint_opt(end_frame)
}

/// Publisher's acknowledgement on the Subscribe Stream for drafts 01-04.
///
/// Lite05+ replaced this with implicit acceptance plus
/// [`SubscribeStart`]/[`SubscribeEnd`]; the immutable timescale/cache moved
/// to [`super::TrackInfo`].
#[derive(Clone, Debug)]
pub struct SubscribeOk {
	pub priority: u8,
	pub max_delay: std::time::Duration,
	pub start_group: Option<u64>,
	pub end_group: Option<u64>,
}

impl Message for SubscribeOk {
	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 => {
				w.u8(self.priority);
			}
			Version::Lite02 => {}
			// Lite05+ never sends SUBSCRIBE_OK, but keep the field layout matching
			// Lite03/04 so a stray future use stays well-formed.
			_ => {
				w.u8(self.priority);
				pad_group_order(w, version)?;
				w.varint(u64::try_from(self.max_delay.as_millis()).map_err(|_| EncodeError::BoundsExceeded)?)?;
				w.varint_opt(self.start_group)?;
				w.varint_opt(self.end_group)?;
			}
		}

		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 => Ok(Self {
				priority: r.u8()?,
				max_delay: std::time::Duration::ZERO,
				start_group: None,
				end_group: None,
			}),
			Version::Lite02 => Ok(Self {
				priority: 0,
				max_delay: std::time::Duration::ZERO,
				start_group: None,
				end_group: None,
			}),
			_ => {
				let priority = r.u8()?;
				skip_group_order(r, version)?;
				let max_delay = std::time::Duration::from_millis(r.varint()?);
				let start_group = r.varint_opt()?;
				let end_group = r.varint_opt()?;

				Ok(Self {
					priority,
					max_delay,
					start_group,
					end_group,
				})
			}
		}
	}
}

/// Resolves the absolute start group of a Lite05+ subscription. The first message
/// the publisher sends, once the start group is known. A value greater than the
/// requested start implicitly drops the leading range.
///
/// There is no start *frame*: a partial group is only served to a subscriber that asked
/// for one, so delivery begins either at the requested `Frame Start` (when this is the
/// requested group) or at frame 0 (when the publisher resolved to a later one). A
/// subscriber that asked for group 5 frame 15 and receives group 6 starts at frame 0.
#[derive(Clone, Debug)]
pub struct SubscribeStart {
	pub group: u64,
	/// The publisher's largest (group, frame) when it answered, `None` for a track with
	/// nothing yet. Lite07+ only; not on the wire before, where it decodes as `None`.
	pub largest: Option<crate::track::Position>,
}

impl Message for SubscribeStart {
	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_track_stream() {
			return Err(DecodeError::Version);
		}
		let group = r.varint()?;
		let largest = match version.has_largest() {
			// Group + 1, so 0 is a track with nothing yet; the frame follows only otherwise.
			true => match r.varint_opt()? {
				Some(group) => Some(crate::track::Position {
					group,
					frame: r.varint()?,
				}),
				None => None,
			},
			false => None,
		};
		Ok(Self { group, largest })
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_track_stream() {
			return Err(EncodeError::Version);
		}
		w.varint(self.group)?;
		if version.has_largest() {
			w.varint_opt(self.largest.map(|largest| largest.group))?;
			if let Some(largest) = self.largest {
				w.varint(largest.frame)?;
			}
		}
		Ok(())
	}
}

/// Signals the exclusive end of a Lite05+ subscription.
///
/// No group at or after `group` will be produced. `0` means the track ended
/// before producing any groups.
#[derive(Clone, Debug)]
pub struct SubscribeEnd {
	pub group: u64,
	/// The number of group streams the publisher opened for this subscription.
	/// Lite07+ only; not on the wire before, where it decodes as 0.
	pub streams: u64,
}

impl Message for SubscribeEnd {
	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_track_stream() {
			return Err(DecodeError::Version);
		}
		let group = r.varint()?;
		let streams = match version.has_stream_count() {
			true => r.varint()?,
			false => 0,
		};
		Ok(Self { group, streams })
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_track_stream() {
			return Err(EncodeError::Version);
		}
		w.varint(self.group)?;
		if version.has_stream_count() {
			w.varint(self.streams)?;
		}
		Ok(())
	}
}

/// Sent by the subscriber to update subscription parameters.
///
/// Lite03+ only.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct SubscribeUpdate {
	pub priority: u8,
	pub max_delay: std::time::Duration,
	pub start_group: Option<u64>,
	pub end_group: Option<u64>,
	/// See [`Subscribe::start_frame`].
	pub start_frame: u64,
	/// See [`Subscribe::end_frame`].
	pub end_frame: Option<u64>,
}

impl Message for SubscribeUpdate {
	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {
				return Err(DecodeError::Version);
			}
			_ => {}
		}

		let priority = r.u8()?;
		skip_group_order(r, version)?;
		let max_delay = std::time::Duration::from_millis(r.varint()?);
		let start_group = decode_start_group(r, version)?;
		let end_group = match r.varint()? {
			0 => None,
			group => Some(group - 1),
		};

		let (start_frame, end_frame) = decode_frame_bounds(r, version, start_group, end_group)?;
		let start_group = canonical_start_group(version, start_group, start_frame);

		Ok(Self {
			priority,
			max_delay,
			start_group,
			end_group,
			start_frame,
			end_frame,
		})
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {
				return Err(EncodeError::Version);
			}
			_ => {}
		}

		w.u8(self.priority);
		pad_group_order(w, version)?;
		w.varint(u64::try_from(self.max_delay.as_millis()).map_err(|_| EncodeError::BoundsExceeded)?)?;

		encode_start_group(w, version, self.start_group)?;

		w.varint_opt(self.end_group)?;

		encode_frame_bounds(
			w,
			version,
			self.start_group,
			self.start_frame,
			self.end_group,
			self.end_frame,
		)?;

		Ok(())
	}
}

/// Indicates that one or more groups have been dropped.
///
/// The range `[start, end]` is inclusive on both ends. For example,
/// `start = 5, end = 7` means groups 5, 6, and 7 were dropped.
///
/// Lite03 to Lite06 only: Lite07 counts group streams in [`SubscribeEnd`] instead.
#[derive(Clone, Debug)]
pub struct SubscribeDrop {
	/// The first absolute group sequence in the dropped range.
	pub start: u64,

	/// The last absolute group sequence in the dropped range (inclusive).
	pub end: u64,

	/// An application-specific error code. A value of 0 indicates no error;
	/// the groups are simply unavailable.
	pub error: u64,
}

impl Message for SubscribeDrop {
	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {
				return Err(DecodeError::Version);
			}
			_ if version.has_stream_count() => return Err(DecodeError::Version),
			_ => {}
		}

		Ok(Self {
			start: r.varint()?,
			end: r.varint()?,
			error: r.varint()?,
		})
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {
				return Err(EncodeError::Version);
			}
			_ if version.has_stream_count() => return Err(EncodeError::Version),
			_ => {}
		}

		w.varint(self.start)?;
		w.varint(self.end)?;
		w.varint(self.error)?;

		Ok(())
	}
}

/// A response message on the subscribe stream, prefixed with a type discriminator
/// on Lite03+.
///
/// The discriminator is version-dependent:
/// - Lite03/04: `0x0` SUBSCRIBE_OK, `0x1` SUBSCRIBE_DROP.
/// - Lite05/06: `0x0` SUBSCRIBE_START, `0x1` SUBSCRIBE_END, `0x2` SUBSCRIBE_DROP
///   (SUBSCRIBE_OK was removed; acceptance is implicit).
/// - Lite07+: `0x0` SUBSCRIBE_START, `0x1` SUBSCRIBE_END (SUBSCRIBE_DROP was removed).
#[derive(Clone, Debug)]
pub enum SubscribeResponse {
	Ok(SubscribeOk),
	Start(SubscribeStart),
	End(SubscribeEnd),
	Drop(SubscribeDrop),
}

/// Write a `type` varint followed by the size-prefixed message body.
fn encode_typed<M: Message>(w: &mut Encoder<'_>, typ: u64, msg: &M, version: Version) -> Result<(), EncodeError> {
	w.varint(typ)?;
	msg.encode(w, version)
}

impl Encode<Version> for SubscribeResponse {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => match self {
				Self::Ok(ok) => ok.encode(w, version)?,
				_ => return Err(EncodeError::Version),
			},
			Version::Lite03 | Version::Lite04 => match self {
				Self::Ok(ok) => encode_typed(w, 0, ok, version)?,
				Self::Drop(drop) => encode_typed(w, 1, drop, version)?,
				_ => return Err(EncodeError::Version),
			},
			// Lite05+: SUBSCRIBE_OK is gone; START/END/DROP carry the resolved range.
			_ => match self {
				Self::Start(start) => encode_typed(w, 0, start, version)?,
				Self::End(end) => encode_typed(w, 1, end, version)?,
				Self::Drop(drop) if !version.has_stream_count() => encode_typed(w, 2, drop, version)?,
				Self::Drop(_) | Self::Ok(_) => return Err(EncodeError::Version),
			},
		}

		Ok(())
	}
}

impl Decode<Version> for SubscribeResponse {
	fn decode(buf: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => Ok(Self::Ok(SubscribeOk::decode(buf, version)?)),
			Version::Lite03 | Version::Lite04 => {
				let typ = buf.varint()?;
				match typ {
					0 => Ok(Self::Ok(SubscribeOk::decode(buf, version)?)),
					1 => Ok(Self::Drop(SubscribeDrop::decode(buf, version)?)),
					_ => Err(DecodeError::InvalidMessage(typ)),
				}
			}
			_ => {
				let typ = buf.varint()?;
				match typ {
					0 => Ok(Self::Start(SubscribeStart::decode(buf, version)?)),
					1 => Ok(Self::End(SubscribeEnd::decode(buf, version)?)),
					2 if !version.has_stream_count() => Ok(Self::Drop(SubscribeDrop::decode(buf, version)?)),
					_ => Err(DecodeError::InvalidMessage(typ)),
				}
			}
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn subscribe_start_roundtrips_on_lite05() {
		let resp = SubscribeResponse::Start(SubscribeStart {
			group: 42,
			largest: None,
		});
		let mut buf = Vec::new();
		resp.encode(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05)
			.unwrap();
		let mut slice = buf.as_slice();
		match crate::coding::decode_buf(&mut slice, Version::Lite05, SubscribeResponse::decode).unwrap() {
			SubscribeResponse::Start(start) => assert_eq!(start.group, 42),
			other => panic!("expected Start, got {other:?}"),
		}
	}

	/// Lite-07 carries the publisher's largest position; earlier versions leave it off the
	/// wire, so it decodes as `None` there.
	#[test]
	fn subscribe_start_carries_the_largest_position_on_lite07() {
		for largest in [None, Some(crate::track::Position { group: 3, frame: 2 })] {
			let resp = SubscribeResponse::Start(SubscribeStart { group: 4, largest });
			let mut buf = Vec::new();
			resp.encode(
				&mut crate::coding::Encoder::new(&mut buf, Version::Lite07.into()),
				Version::Lite07,
			)
			.unwrap();
			match SubscribeResponse::decode_slice(&buf, Version::Lite07).unwrap().0 {
				SubscribeResponse::Start(start) => assert_eq!((start.group, start.largest), (4, largest)),
				other => panic!("expected Start, got {other:?}"),
			}
		}
		let resp = SubscribeResponse::Start(SubscribeStart {
			group: 4,
			largest: Some(crate::track::Position { group: 3, frame: 2 }),
		});
		let mut buf = Vec::new();
		resp.encode(
			&mut crate::coding::Encoder::new(&mut buf, Version::Lite06.into()),
			Version::Lite06,
		)
		.unwrap();
		assert_eq!(buf, [0, 1, 4], "lite-06 has no largest position");
	}

	#[test]
	fn subscribe_end_roundtrips_on_lite05() {
		let resp = SubscribeResponse::End(SubscribeEnd { group: 7, streams: 3 });
		let mut buf = Vec::new();
		resp.encode(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05)
			.unwrap();
		// Type, length, group: no stream count before lite-07.
		assert_eq!(buf, [1, 1, 7]);
		let mut slice = buf.as_slice();
		match crate::coding::decode_buf(&mut slice, Version::Lite05, SubscribeResponse::decode).unwrap() {
			SubscribeResponse::End(end) => assert_eq!((end.group, end.streams), (7, 0)),
			other => panic!("expected End, got {other:?}"),
		}
	}

	#[test]
	fn subscribe_end_carries_the_stream_count_on_lite07() {
		let resp = SubscribeResponse::End(SubscribeEnd { group: 7, streams: 3 });
		let mut buf = Vec::new();
		resp.encode(&mut Encoder::new(&mut buf, Version::Lite07.into()), Version::Lite07)
			.unwrap();
		assert_eq!(buf, [1, 2, 7, 3]);
		let mut slice = buf.as_slice();
		match crate::coding::decode_buf(&mut slice, Version::Lite07, SubscribeResponse::decode).unwrap() {
			SubscribeResponse::End(end) => assert_eq!((end.group, end.streams), (7, 3)),
			other => panic!("expected End, got {other:?}"),
		}
	}

	#[test]
	fn subscribe_drop_is_gone_on_lite07() {
		let resp = SubscribeResponse::Drop(SubscribeDrop {
			start: 1,
			end: 3,
			error: 0,
		});
		let mut buf = Vec::new();
		assert!(matches!(
			resp.encode(&mut Encoder::new(&mut buf, Version::Lite07.into()), Version::Lite07),
			Err(EncodeError::Version)
		));

		// A lite-06 DROP is an unknown response type on lite-07.
		let mut buf = Vec::new();
		resp.encode(&mut Encoder::new(&mut buf, Version::Lite06.into()), Version::Lite06)
			.unwrap();
		assert!(matches!(
			crate::coding::decode_buf(&mut buf.as_slice(), Version::Lite07, SubscribeResponse::decode),
			Err(DecodeError::InvalidMessage(2))
		));
	}

	#[test]
	fn subscribe_drop_is_type_2_on_lite05() {
		let resp = SubscribeResponse::Drop(SubscribeDrop {
			start: 1,
			end: 3,
			error: 0,
		});
		let mut buf = Vec::new();
		resp.encode(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05)
			.unwrap();
		// Type discriminator is the first varint; on Lite05 DROP is 0x2.
		assert_eq!(buf[0], 2);

		let mut slice = buf.as_slice();
		match crate::coding::decode_buf(&mut slice, Version::Lite05, SubscribeResponse::decode).unwrap() {
			SubscribeResponse::Drop(drop) => assert_eq!((drop.start, drop.end), (1, 3)),
			other => panic!("expected Drop, got {other:?}"),
		}
	}

	#[test]
	fn subscribe_drop_is_type_1_on_lite04() {
		let resp = SubscribeResponse::Drop(SubscribeDrop {
			start: 1,
			end: 3,
			error: 0,
		});
		let mut buf = Vec::new();
		resp.encode(&mut Encoder::new(&mut buf, Version::Lite04.into()), Version::Lite04)
			.unwrap();
		assert_eq!(buf[0], 1);
	}

	fn subscribe_sample() -> Subscribe<'static> {
		Subscribe {
			epoch: None,
			id: 1,
			broadcast: Path::new("room").to_owned(),
			track: Cow::Borrowed("video"),
			priority: 3,
			max_delay: std::time::Duration::from_millis(250),
			start_group: Some(7),
			end_group: Some(9),
			start_frame: 4,
			end_frame: Some(2),
		}
	}

	#[test]
	fn subscribe_frame_bounds_roundtrip() {
		let msg = subscribe_sample();
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite06.into()), Version::Lite06)
			.unwrap();
		let got = crate::coding::decode_buf(&mut buf.as_slice(), Version::Lite06, Subscribe::decode_msg).unwrap();
		assert_eq!((got.start_group, got.start_frame), (Some(7), 4));
		assert_eq!((got.end_group, got.end_frame), (Some(9), Some(2)));
	}

	/// The whole-group defaults are what a version without the fields decodes to, so
	/// lite-05 stays byte-identical. Compared without a floor, since `Group Start` itself
	/// encodes differently across the two (see `group_start_is_absolute_on_lite06`).
	#[test]
	fn subscribe_drops_the_retired_ordered_byte_on_lite06() {
		let mut msg = subscribe_sample();
		msg.start_group = None;
		msg.start_frame = 0;
		msg.end_frame = None;

		let mut lite05 = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut lite05, Version::Lite05.into()), Version::Lite05)
			.unwrap();
		let mut lite06 = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut lite06, Version::Lite06.into()), Version::Lite06)
			.unwrap();

		// The two layouts diverge in exactly one place: the retired byte lite-05 still
		// reserves. A deployed peer's field offsets depend on it being there and zero.
		let ordered_at = lite05
			.iter()
			.zip(&lite06)
			.position(|(a, b)| a != b)
			.expect("the layouts must diverge at the retired byte");
		assert_eq!(lite05[ordered_at], 0, "the retired byte is written as zero");

		// Remove it and lite-06 is the same message plus the two defaulted frame varints.
		let mut spliced = lite05.clone();
		spliced.remove(ordered_at);
		assert_eq!(&lite06[..spliced.len()], &spliced[..]);
		assert_eq!(&lite06[spliced.len()..], &[0, 0]);

		let got = crate::coding::decode_buf(&mut lite05.as_slice(), Version::Lite05, Subscribe::decode_msg).unwrap();
		assert_eq!((got.start_frame, got.end_frame), (0, None));
	}

	/// Lite06 carries the raw floor; pre-06 wires encode the sequence + 1 with 0 meaning
	/// the latest group. A vacuous floor folds to absent on the old wire, where an
	/// explicit group 0 would mean "replay from the beginning" instead.
	#[test]
	fn group_start_is_absolute_on_lite06() {
		let mut msg = subscribe_sample();
		msg.start_frame = 0;
		msg.end_frame = None;

		let mut lite05 = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut lite05, Version::Lite05.into()), Version::Lite05)
			.unwrap();
		let mut lite06 = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut lite06, Version::Lite06.into()), Version::Lite06)
			.unwrap();

		let on05 = crate::coding::decode_buf(&mut lite05.as_slice(), Version::Lite05, Subscribe::decode_msg).unwrap();
		let on06 = crate::coding::decode_buf(&mut lite06.as_slice(), Version::Lite06, Subscribe::decode_msg).unwrap();
		assert_eq!(on05.start_group, Some(7));
		assert_eq!(on06.start_group, Some(7));
		// The raw byte differs: 7 on the wire, not 7 + 1.
		assert_ne!(lite05, lite06);

		// No floor and a floor of 0 are the same absence of a constraint on lite-06:
		// byte-identical on the wire, and canonicalized to absent on decode.
		msg.start_group = None;
		let mut absent = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut absent, Version::Lite06.into()), Version::Lite06)
			.unwrap();
		msg.start_group = Some(0);
		let mut zero = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut zero, Version::Lite06.into()), Version::Lite06)
			.unwrap();
		assert_eq!(absent, zero);
		let got = crate::coding::decode_buf(&mut zero.as_slice(), Version::Lite06, Subscribe::decode_msg).unwrap();
		assert_eq!(got.start_group, None);

		// On the pre-06 wire the vacuous floor folds to absent (the latest group).
		let mut folded = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut folded, Version::Lite05.into()), Version::Lite05)
			.unwrap();
		let got = crate::coding::decode_buf(&mut folded.as_slice(), Version::Lite05, Subscribe::decode_msg).unwrap();
		assert_eq!(got.start_group, None);
	}

	/// A subscription can resume partway through group 0 (a catalog never leaves it), so
	/// a `Frame Start` qualifying the zero floor must survive the round trip.
	#[test]
	fn frame_start_may_qualify_group_zero_on_lite06() {
		let mut msg = subscribe_sample();
		msg.start_group = Some(0);
		msg.start_frame = 4;

		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite06.into()), Version::Lite06)
			.unwrap();
		let got = crate::coding::decode_buf(&mut buf.as_slice(), Version::Lite06, Subscribe::decode_msg).unwrap();
		assert_eq!((got.start_group, got.start_frame), (Some(0), 4));
	}

	/// Silently widening to the whole group would deliver frames the caller excluded.
	#[test]
	fn subscribe_frame_bounds_rejected_before_lite06() {
		let mut buf = Vec::new();
		assert!(
			subscribe_sample()
				.encode_msg(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05)
				.is_err()
		);
	}

	/// Frames are numbered per group, so a frame bound without its group bound has
	/// nothing to count from.
	#[test]
	fn subscribe_frame_bound_without_group_bound_is_invalid() {
		let mut msg = subscribe_sample();
		msg.start_group = None;
		msg.start_frame = 4;
		msg.end_group = None;
		msg.end_frame = None;

		let mut buf = Vec::new();
		assert!(matches!(
			msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite06.into()), Version::Lite06),
			Err(EncodeError::InvalidState)
		));

		msg.start_frame = 0;
		msg.end_frame = Some(7);
		assert!(matches!(
			msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite06.into()), Version::Lite06),
			Err(EncodeError::InvalidState)
		));
	}

	#[test]
	fn subscribe_ok_rejected_on_lite05() {
		let resp = SubscribeResponse::Ok(SubscribeOk {
			priority: 1,
			max_delay: std::time::Duration::ZERO,
			start_group: None,
			end_group: None,
		});
		let mut buf = Vec::new();
		assert!(
			resp.encode(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05)
				.is_err()
		);
	}
}
