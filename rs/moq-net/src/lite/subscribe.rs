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
	/// Where delivery may start; see [`Start`].
	pub start: Start,
	pub end_group: Option<u64>,
	/// Last frame to deliver (inclusive) within `end_group`'s group, or `None` for the
	/// whole group. Lite06+ only, and meaningless without an explicit `end_group`.
	pub end_frame: Option<u64>,
}

/// Where a subscription may start: the live edge, a floor, or both, as SUBSCRIBE and
/// SUBSCRIBE_UPDATE carry them. See [`crate::track::Subscription::live`].
///
/// Lite-07 carries both fields. Older versions have only `Group Start`, so the codec folds
/// `live` into it (see [`crate::track::Subscription::folded_floor`]) and reads it back:
/// lite-06 `Group Start` 0 with `Frame Start` 0 is `live`, and any other pair a floor
/// (`(0, N)` is a catalog resume); a pre-06 absent `Group Start` is `live`, and lite-01/02
/// carry none at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Start {
	/// Deliver from the live edge.
	pub live: bool,
	/// The lowest position to deliver, or `None` for no floor.
	pub floor: Option<crate::track::Position>,
}

impl Start {
	/// Live alone, which is what a wire without a `Group Start` asks for.
	pub const LIVE: Self = Self {
		live: true,
		floor: None,
	};

	/// The subscription's start. Neither `live` nor a floor asks for nothing.
	pub(crate) fn of(subscription: &crate::track::Subscription) -> Self {
		Self {
			live: subscription.live,
			floor: subscription.floor,
		}
	}

	/// The floor a version without `Live` carries; see [`Self`].
	fn folded(self) -> Option<crate::track::Position> {
		crate::track::Subscription::default()
			.with_live(self.live)
			.with_floor(self.floor)
			.folded_floor()
	}

	/// Decode the start of a SUBSCRIBE or SUBSCRIBE_UPDATE: everything on lite-07 and
	/// pre-06, and the `Group Start` half on lite-06, whose `Frame Start` trails the end
	/// group and is applied by [`Self::decode_frame`].
	fn decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		if version.has_live() {
			let live = r.bool()?;
			let floor = match r.bool()? {
				true => Some(crate::track::Position {
					group: r.varint()?,
					frame: r.varint()?,
				}),
				false => None,
			};
			if !live && floor.is_none() {
				return Err(DecodeError::InvalidSubscribeLocation);
			}
			return Ok(Self { live, floor });
		}
		if version.resolves_start() {
			return Ok(Self::floored(crate::track::Position::group(r.varint()?)));
		}
		Ok(match r.varint_opt()? {
			Some(group) => Self::floored(crate::track::Position::group(group)),
			None => Self::LIVE,
		})
	}

	/// Apply lite-06's trailing `Frame Start`, then read the pair back: `(0, 0)` is `live`.
	fn decode_frame(self, r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		if version.has_live() || !version.has_frame_bounds() {
			return Ok(self);
		}
		let frame = r.varint()?;
		Ok(match self.floor {
			Some(floor) if floor.group == 0 && frame == 0 => Self::LIVE,
			Some(floor) => Self::floored(crate::track::Position {
				group: floor.group,
				frame,
			}),
			None => self,
		})
	}

	pub(crate) fn floored(floor: crate::track::Position) -> Self {
		Self {
			live: false,
			floor: Some(floor),
		}
	}

	/// Encode what [`Self::decode`] reads.
	///
	/// Neither `live` nor a floor has no encoding, since it asks for nothing. A pre-06
	/// `Group Start` cannot qualify a frame, so a floor partway through a group is refused
	/// rather than widened to the whole group.
	fn encode(self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !self.live && self.floor.is_none() {
			return Err(EncodeError::InvalidState);
		}
		if version.has_live() {
			w.bool(self.live);
			w.bool(self.floor.is_some());
			if let Some(floor) = self.floor {
				w.varint(floor.group)?;
				w.varint(floor.frame)?;
			}
			return Ok(());
		}
		let folded = self.folded();
		if version.resolves_start() {
			return w.varint(folded.map_or(0, |floor| floor.group));
		}
		if folded.is_some_and(|floor| floor.frame != 0) {
			return Err(EncodeError::Version);
		}
		// The sequence + 1, so an explicit group 0 (replay from the beginning) is 1.
		w.varint_opt(folded.map(|floor| floor.group))
	}

	/// Encode lite-06's trailing `Frame Start`; see [`Self::decode_frame`].
	fn encode_frame(self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if version.has_live() || !version.has_frame_bounds() {
			return Ok(());
		}
		w.varint(self.folded().map_or(0, |floor| floor.frame))
	}
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

		let (max_delay, start, end_group) = match version {
			Version::Lite01 | Version::Lite02 => (std::time::Duration::ZERO, Start::LIVE, None),
			_ => {
				skip_group_order(r, version)?;
				let max_delay = std::time::Duration::from_millis(r.varint()?);
				let start = Start::decode(r, version)?;
				let end_group = r.varint_opt()?;
				(max_delay, start, end_group)
			}
		};

		let start = start.decode_frame(r, version)?;
		let end_frame = decode_end_frame(r, version, end_group)?;

		Ok(Self {
			id,
			broadcast,
			epoch,
			track,
			priority,
			max_delay,
			start,
			end_group,
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
				self.start.encode(w, version)?;
				w.varint_opt(self.end_group)?;
			}
		}

		self.start.encode_frame(w, version)?;
		encode_end_frame(w, version, self.end_group, self.end_frame)
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

/// Decode the trailing `Frame End` shared by SUBSCRIBE and SUBSCRIBE_UPDATE.
///
/// Older versions carry no such field, so they decode as the whole group. A frame bound
/// without the group bound it qualifies is a protocol violation: frames are numbered per
/// group, so there is nothing to count from.
fn decode_end_frame(r: &mut Decoder<'_>, version: Version, end_group: Option<u64>) -> Result<Option<u64>, DecodeError> {
	if !version.has_frame_bounds() {
		return Ok(None);
	}

	let end_frame = r.varint_opt()?;
	if end_frame.is_some() && end_group.is_none() {
		return Err(DecodeError::InvalidSubscribeLocation);
	}
	Ok(end_frame)
}

/// Encode the trailing `Frame End`, a no-op before lite-06.
fn encode_end_frame(
	w: &mut Encoder<'_>,
	version: Version,
	end_group: Option<u64>,
	end_frame: Option<u64>,
) -> Result<(), EncodeError> {
	if end_frame.is_some() && end_group.is_none() {
		return Err(EncodeError::InvalidState);
	}

	if !version.has_frame_bounds() {
		// Nothing carries the bound, so silently widening to the whole group would
		// deliver frames the caller excluded. Refuse instead.
		if end_frame.is_some() {
			return Err(EncodeError::Version);
		}
		return Ok(());
	}

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
/// Lite03+ only. Every field replaces the subscription's, so an update clears a floor or
/// `live` by leaving it out.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct SubscribeUpdate {
	pub priority: u8,
	pub max_delay: std::time::Duration,
	/// See [`Subscribe::start`].
	pub start: Start,
	pub end_group: Option<u64>,
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
		let start = Start::decode(r, version)?;
		let end_group = r.varint_opt()?;
		let start = start.decode_frame(r, version)?;
		let end_frame = decode_end_frame(r, version, end_group)?;

		Ok(Self {
			priority,
			max_delay,
			start,
			end_group,
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
		self.start.encode(w, version)?;
		w.varint_opt(self.end_group)?;
		self.start.encode_frame(w, version)?;
		encode_end_frame(w, version, self.end_group, self.end_frame)
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

	use crate::track::Position;

	fn subscribe_sample() -> Subscribe<'static> {
		Subscribe {
			epoch: None,
			id: 1,
			broadcast: Path::new("room").to_owned(),
			track: Cow::Borrowed("video"),
			priority: 3,
			max_delay: std::time::Duration::from_millis(250),
			start: Start::floored(Position { group: 7, frame: 4 }),
			end_group: Some(9),
			end_frame: Some(2),
		}
	}

	fn encode(msg: &Subscribe<'_>, version: Version) -> Result<Vec<u8>, EncodeError> {
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)?;
		Ok(buf)
	}

	fn decode(buf: &[u8], version: Version) -> Result<Subscribe<'static>, DecodeError> {
		crate::coding::decode_buf(&mut &buf[..], version, Subscribe::decode_msg)
	}

	/// What `start` reads back as on `version`, through SUBSCRIBE and SUBSCRIBE_UPDATE.
	fn roundtrip(start: Start, version: Version) -> Start {
		let msg = Subscribe {
			start,
			end_group: None,
			end_frame: None,
			..subscribe_sample()
		};
		let got = decode(&encode(&msg, version).unwrap(), version).unwrap().start;

		let update = SubscribeUpdate {
			priority: 0,
			max_delay: std::time::Duration::ZERO,
			start,
			end_group: None,
			end_frame: None,
		};
		let mut buf = Vec::new();
		update
			.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		let updated = crate::coding::decode_buf(&mut buf.as_slice(), version, SubscribeUpdate::decode_msg)
			.unwrap()
			.start;
		assert_eq!(got, updated, "{version:?}: SUBSCRIBE and SUBSCRIBE_UPDATE agree");
		got
	}

	fn both(floor: Position) -> Start {
		Start {
			live: true,
			floor: Some(floor),
		}
	}

	#[test]
	fn subscribe_frame_bounds_roundtrip() {
		let msg = subscribe_sample();
		for version in [Version::Lite06, Version::Lite07] {
			let got = decode(&encode(&msg, version).unwrap(), version).unwrap();
			assert_eq!(got.start, msg.start, "{version:?}");
			assert_eq!((got.end_group, got.end_frame), (Some(9), Some(2)), "{version:?}");
		}
	}

	/// Lite-07 carries `Live` and an optional floor separately, so every combination that
	/// asks for something survives the round trip, including a floor of (0, 0).
	#[test]
	fn lite07_carries_live_beside_the_floor() {
		for start in [
			Start::LIVE,
			Start::floored(Position::group(0)),
			Start::floored(Position { group: 3, frame: 5 }),
			both(Position::group(0)),
			both(Position { group: 4, frame: 2 }),
		] {
			assert_eq!(roundtrip(start, Version::Lite07), start);
		}

		// `Live`, then the floor's presence, then the floor itself.
		let msg = Subscribe {
			start: both(Position { group: 4, frame: 2 }),
			end_group: None,
			end_frame: None,
			..subscribe_sample()
		};
		let buf = encode(&msg, Version::Lite07).unwrap();
		let without_floor = encode(
			&Subscribe {
				start: Start::LIVE,
				..msg.clone()
			},
			Version::Lite07,
		)
		.unwrap();
		assert_eq!(buf.len(), without_floor.len() + 2, "the floor's two varints are gone");
		assert!(buf.ends_with(&[1, 1, 4, 2, 0, 0]), "{buf:?}");
		assert!(without_floor.ends_with(&[1, 0, 0, 0]), "{without_floor:?}");
	}

	/// Neither `Live` nor a floor asks for nothing, so it has no encoding and decoding one is
	/// a protocol violation.
	#[test]
	fn lite07_refuses_neither_live_nor_a_floor() {
		let neither = Subscribe {
			start: Start::default(),
			..subscribe_sample()
		};
		assert!(matches!(
			encode(&neither, Version::Lite07),
			Err(EncodeError::InvalidState)
		));

		let mut buf = encode(
			&Subscribe {
				start: Start::LIVE,
				end_group: None,
				end_frame: None,
				..subscribe_sample()
			},
			Version::Lite07,
		)
		.unwrap();
		// Clear `Live`, which sits just before the absent floor and the two end varints.
		let live_at = buf.len() - 4;
		assert_eq!(buf[live_at], 1);
		buf[live_at] = 0;
		assert!(matches!(
			decode(&buf, Version::Lite07),
			Err(DecodeError::InvalidSubscribeLocation)
		));
		// Any other byte is not a boolean.
		buf[live_at] = 2;
		assert!(matches!(decode(&buf, Version::Lite07), Err(DecodeError::InvalidValue)));
	}

	/// Lite-06 has no `Live`: `Group Start` 0 with `Frame Start` 0 is `live`, and any other
	/// pair a floor, so `live` with a floor goes out as (0, 0) and the receiver filters.
	#[test]
	fn lite06_folds_live_into_group_start_zero() {
		let version = Version::Lite06;
		assert_eq!(roundtrip(Start::LIVE, version), Start::LIVE);
		assert_eq!(roundtrip(both(Position::group(4)), version), Start::LIVE);
		let floor = Start::floored(Position { group: 7, frame: 4 });
		assert_eq!(roundtrip(floor, version), floor);
		// A catalog resume partway through group 0 stays a floor.
		let catalog = Start::floored(Position { group: 0, frame: 4 });
		assert_eq!(roundtrip(catalog, version), catalog);
		// A floor of (0, 0) reads back as `live`, which lite-06 cannot tell apart.
		assert_eq!(roundtrip(Start::floored(Position::group(0)), version), Start::LIVE);

		// Byte-identical to what lite-06 always sent for "no floor".
		let live = Subscribe {
			start: Start::LIVE,
			..subscribe_sample()
		};
		let folded = Subscribe {
			start: both(Position::group(4)),
			..subscribe_sample()
		};
		assert_eq!(encode(&live, version).unwrap(), encode(&folded, version).unwrap());
	}

	/// Pre-06 wires encode `Group Start` as the sequence + 1, with 0 meaning the latest
	/// group: absent is `live`, and `live` with a floor goes out as an explicit group 0,
	/// replay from the beginning, so the receiver can filter locally.
	#[test]
	fn pre06_folds_live_into_an_explicit_group_zero() {
		for version in [Version::Lite03, Version::Lite04, Version::Lite05] {
			assert_eq!(roundtrip(Start::LIVE, version), Start::LIVE, "{version:?}");
			assert_eq!(
				roundtrip(both(Position::group(2)), version),
				Start::floored(Position::group(0)),
				"{version:?}"
			);
			let floor = Start::floored(Position::group(7));
			assert_eq!(roundtrip(floor, version), floor, "{version:?}");
			// An explicit group 0 is 1 on the wire, not folded back to absent.
			assert_eq!(
				roundtrip(Start::floored(Position::group(0)), version),
				Start::floored(Position::group(0)),
				"{version:?}"
			);
		}

		let msg = Subscribe {
			start: Start::floored(Position::group(7)),
			end_frame: None,
			..subscribe_sample()
		};
		let lite05 = encode(&msg, Version::Lite05).unwrap();
		let lite06 = encode(&msg, Version::Lite06).unwrap();
		assert_ne!(lite05, lite06, "7 + 1 on lite-05, 7 on lite-06");
	}

	/// Lite-01 and 02 carry no `Group Start`, so every subscription decodes as `live`.
	#[test]
	fn lite01_carries_no_start() {
		for version in [Version::Lite01, Version::Lite02] {
			let msg = Subscribe {
				start: Start::floored(Position::group(7)),
				end_group: None,
				end_frame: None,
				..subscribe_sample()
			};
			assert_eq!(
				decode(&encode(&msg, version).unwrap(), version).unwrap().start,
				Start::LIVE
			);
		}
	}

	/// The whole-group defaults are what a version without the fields decodes to, so
	/// lite-05 stays byte-identical. Compared without a floor, since `Group Start` itself
	/// encodes differently across the two.
	#[test]
	fn subscribe_drops_the_retired_ordered_byte_on_lite06() {
		let msg = Subscribe {
			start: Start::LIVE,
			end_frame: None,
			..subscribe_sample()
		};

		let lite05 = encode(&msg, Version::Lite05).unwrap();
		let lite06 = encode(&msg, Version::Lite06).unwrap();

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

		let got = decode(&lite05, Version::Lite05).unwrap();
		assert_eq!((got.start, got.end_frame), (Start::LIVE, None));
	}

	/// Silently widening to the whole group would deliver frames the caller excluded.
	#[test]
	fn subscribe_frame_bounds_rejected_before_lite06() {
		assert!(matches!(
			encode(&subscribe_sample(), Version::Lite05),
			Err(EncodeError::Version)
		));
		let mid_group = Subscribe {
			end_frame: None,
			..subscribe_sample()
		};
		assert!(matches!(encode(&mid_group, Version::Lite05), Err(EncodeError::Version)));
	}

	/// Frames are numbered per group, so a frame bound without its group bound has
	/// nothing to count from.
	#[test]
	fn subscribe_frame_bound_without_group_bound_is_invalid() {
		let msg = Subscribe {
			end_group: None,
			end_frame: Some(7),
			..subscribe_sample()
		};
		assert!(matches!(encode(&msg, Version::Lite06), Err(EncodeError::InvalidState)));
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
