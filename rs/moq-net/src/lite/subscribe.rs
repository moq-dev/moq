use std::borrow::Cow;

use crate::{
	Path, StreamError,
	coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder, StreamCodes},
};

use super::{Message, Version};
use crate::track::Order;

/// Sent by the subscriber to request the groups of a track within its ranges.
///
/// Objects will use the provided ID instead of the full track name, to save bytes.
#[derive(Clone, Debug)]
pub struct Subscribe<'a> {
	pub id: u64,
	pub broadcast: Path,
	/// The publisher instance the subscriber expects; see [`crate::origin::Route::epoch`].
	/// Lite07+ only.
	pub epoch: Option<crate::Epoch>,
	pub track: Cow<'a, str>,
	pub priority: i8,
	pub max_delay: std::time::Duration,
	/// The spans to deliver, never empty. Lite07+ carries any number; earlier wires carry
	/// exactly one (see [`Range`]).
	pub ranges: Vec<Range>,
	/// Which pending group to send first. Lite07+ only; earlier wires decode as
	/// [`Order::Descending`] and cannot ask for anything else.
	pub order: Order,
}

impl Subscribe<'_> {
	/// Copy the borrowed track name, so the request outlives the bytes it came from.
	pub fn into_owned(self) -> Subscribe<'static> {
		Subscribe {
			id: self.id,
			broadcast: self.broadcast,
			epoch: self.epoch,
			track: Cow::Owned(self.track.into_owned()),
			priority: self.priority,
			max_delay: self.max_delay,
			ranges: self.ranges,
			order: self.order,
		}
	}
}

/// One end of a [`Range`], as the wire carries it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
	/// A position: a start delivers from it, and an end stops just before it.
	Absolute(crate::track::Position),
	/// The group this many before the latest, included as an end. Lite07+ only, except a
	/// start of 0 (see [`Range`]).
	Relative(u64),
	/// The group containing this presentation time in the track's timescale, included as
	/// an end. Lite07+ only.
	Time(u64),
}

/// A span of a track, as the wire carries it.
///
/// Before lite-07 a subscription carries exactly one, in `Group Start` and `Group End` and
/// their frame bounds. An absent `Group Start` decodes as no start, and so does a lite-06
/// `Group Start` of 0 with a `Frame Start` of 0, the same absence of a floor there. No start
/// and a [`Anchor::Relative`] start of 0 both encode as an absent `Group Start`, which a
/// pre-06 publisher reads as the latest group and a lite-06 one as no floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
	/// Where delivery starts, or `None` for the oldest group the max age allows.
	pub start: Option<Anchor>,
	/// Where delivery stops, or `None` to follow the live edge.
	pub end: Option<Anchor>,
}

impl Range {
	/// Everything from the oldest group the max age allows, onward.
	pub const OLDEST: Self = Self { start: None, end: None };

	/// The span an older wire's four fields name: `end_group` and `end_frame` inclusive, as
	/// they go out.
	#[cfg(test)]
	pub(crate) fn span(
		start_group: Option<u64>,
		start_frame: u64,
		end_group: Option<u64>,
		end_frame: Option<u64>,
	) -> Self {
		use crate::track::Position;
		Self {
			start: start_group.map(|group| {
				Anchor::Absolute(Position {
					group,
					frame: start_frame,
				})
			}),
			end: end_group.map(|group| {
				Anchor::Absolute(match end_frame {
					Some(frame) => Position {
						group,
						frame: frame + 1,
					},
					None => Position::group(group + 1),
				})
			}),
		}
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

impl<'a> Message<'a> for Subscribe<'a> {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::Subscribe);
	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let id = r.varint()?;
		let broadcast = Path::decode(r, version)?;
		let epoch = super::epoch::decode_epoch(r, version)?;
		let track = Cow::Borrowed(r.str()?);
		let priority = super::priority::from_wire(r.u8()?);

		let (order, max_delay, ranges) = match version {
			Version::Lite01 | Version::Lite02 => (Order::Descending, std::time::Duration::ZERO, vec![Range::OLDEST]),
			_ => {
				let order = decode_order(r, version)?;
				let max_delay = std::time::Duration::from_millis(r.varint()?);
				(order, max_delay, decode_ranges(r, version)?)
			}
		};

		Ok(Self {
			id,
			broadcast,
			epoch,
			track,
			priority,
			max_delay,
			ranges,
			order,
		})
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		w.varint(self.id)?;
		self.broadcast.encode(w, version)?;
		super::epoch::encode_epoch(w, version, self.epoch.as_ref())?;
		w.string(&self.track)?;
		w.u8(super::priority::to_wire(self.priority));

		match version {
			// No field carries a range: the publisher serves the live edge.
			Version::Lite01 | Version::Lite02 => {}
			_ => {
				encode_order(w, version, self.order)?;
				w.varint(u64::try_from(self.max_delay.as_millis()).map_err(|_| EncodeError::BoundsExceeded)?)?;
				encode_ranges(w, version, &self.ranges)?;
			}
		}

		Ok(())
	}
}

/// Decode the ranges SUBSCRIBE and SUBSCRIBE_UPDATE carry after the max age.
///
/// Lite-07 carries a non-empty list. Older wires carry one span in `Group Start`,
/// `Group End`, and their frame bounds; see [`Range`].
fn decode_ranges(r: &mut Decoder<'_>, version: Version) -> Result<Vec<Range>, DecodeError> {
	if version.has_ranges() {
		let count = r.varint()?;
		// A subscription that asks for nothing has no use: refuse it rather than park it.
		if count == 0 {
			return Err(DecodeError::InvalidSubscribeLocation);
		}
		// Not preallocated: each range costs at least two bytes of the bounded message, so a
		// count the bytes can't back runs out before it costs anything.
		let mut ranges = Vec::new();
		for _ in 0..count {
			let start = decode_anchor(r)?;
			let end = decode_anchor(r)?;
			ranges.push(Range { start, end });
		}
		return Ok(ranges);
	}

	let start_group = decode_start_group(r, version)?;
	let end_group = r.varint_opt()?;
	let (start_frame, end_frame) = decode_frame_bounds(r, version, start_group, end_group)?;
	let start = canonical_start_group(version, start_group, start_frame).map(|group| {
		Anchor::Absolute(crate::track::Position {
			group,
			frame: start_frame,
		})
	});
	// The wire's end is inclusive, so the exclusive position is just past it. A varint is
	// below 2^62, so neither step overflows.
	let end = end_group.map(|group| {
		Anchor::Absolute(match end_frame {
			Some(frame) => crate::track::Position {
				group,
				frame: frame + 1,
			},
			None => crate::track::Position::group(group + 1),
		})
	});
	Ok(vec![Range { start, end }])
}

/// Encode the inverse of [`decode_ranges`].
///
/// An older wire carries exactly one range, from an absolute position or none, up to an
/// absolute position or the live edge, so anything else is refused for the caller to
/// narrow first.
fn encode_ranges(w: &mut Encoder<'_>, version: Version, ranges: &[Range]) -> Result<(), EncodeError> {
	if version.has_ranges() {
		if ranges.is_empty() {
			return Err(EncodeError::InvalidState);
		}
		w.varint(ranges.len() as u64)?;
		for range in ranges {
			encode_anchor(w, range.start)?;
			encode_anchor(w, range.end)?;
		}
		return Ok(());
	}

	let [range] = ranges else {
		return Err(EncodeError::Version);
	};
	let (start_group, start_frame) = match range.start {
		None | Some(Anchor::Relative(0)) => (None, 0),
		Some(Anchor::Absolute(start)) => (Some(start.group), start.frame),
		Some(Anchor::Relative(_) | Anchor::Time(_)) => return Err(EncodeError::Version),
	};
	let (end_group, end_frame) = match range.end {
		None => (None, None),
		// The wire's end is inclusive: the group before a head-of-group end, or the frame
		// before a mid-group one. Nothing comes before the very first position.
		Some(Anchor::Absolute(end)) => match end.frame.checked_sub(1) {
			Some(frame) => (Some(end.group), Some(frame)),
			None => (Some(end.group.checked_sub(1).ok_or(EncodeError::InvalidState)?), None),
		},
		Some(Anchor::Relative(_) | Anchor::Time(_)) => return Err(EncodeError::Version),
	};
	encode_start_group(w, version, start_group)?;
	w.varint_opt(end_group)?;
	encode_frame_bounds(w, version, start_group, start_frame, end_group, end_frame)
}

/// Decode one lite-07 range end: its type, then the fields that type carries. Type 0 is no
/// anchor, the oldest group as a start and the live edge as an end.
fn decode_anchor(r: &mut Decoder<'_>) -> Result<Option<Anchor>, DecodeError> {
	Ok(match r.varint()? {
		0 => None,
		1 => Some(Anchor::Absolute(crate::track::Position {
			group: r.varint()?,
			frame: r.varint()?,
		})),
		2 => Some(Anchor::Relative(r.varint()?)),
		3 => Some(Anchor::Time(r.varint()?)),
		_ => return Err(DecodeError::InvalidValue),
	})
}

/// Encode the inverse of [`decode_anchor`].
fn encode_anchor(w: &mut Encoder<'_>, anchor: Option<Anchor>) -> Result<(), EncodeError> {
	match anchor {
		None => w.varint(0),
		Some(Anchor::Absolute(position)) => {
			w.varint(1)?;
			w.varint(position.group)?;
			w.varint(position.frame)
		}
		Some(Anchor::Relative(back)) => {
			w.varint(2)?;
			w.varint(back)
		}
		Some(Anchor::Time(pts)) => {
			w.varint(3)?;
			w.varint(pts)
		}
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

/// Decode the byte after the subscriber priority shared by SUBSCRIBE and SUBSCRIBE_UPDATE:
/// lite-07's `Group Order`, or the retired `Ordered` byte an older layout still carries.
fn decode_order(r: &mut Decoder<'_>, version: Version) -> Result<Order, DecodeError> {
	if !version.has_order() {
		skip_group_order(r, version)?;
		return Ok(Order::Descending);
	}
	match r.u8()? {
		0 => Ok(Order::Descending),
		1 => Ok(Order::Ascending),
		_ => Err(DecodeError::InvalidValue),
	}
}

/// Encode the inverse of [`decode_order`]. A wire without `Group Order` serves newest
/// first, so an ascending subscription over it falls back to descending.
fn encode_order(w: &mut Encoder<'_>, version: Version, order: Order) -> Result<(), EncodeError> {
	if !version.has_order() {
		return pad_group_order(w, version);
	}
	w.u8(match order {
		Order::Descending => 0,
		Order::Ascending => 1,
	});
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
/// The inverse of [`decode_start_group`]. Lite-06 writes the raw floor, so `None` and
/// `Some(0)` are the same group 0. A pre-06 wire encodes the sequence + 1: `None` is 0
/// (the latest group, where the publisher starts) and `Some(0)` is 1, replay from the
/// beginning.
fn encode_start_group(w: &mut Encoder<'_>, version: Version, start_group: Option<u64>) -> Result<(), EncodeError> {
	if version.resolves_start() {
		return w.varint(start_group.unwrap_or(0));
	}
	w.varint_opt(start_group)
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
	pub priority: i8,
	pub max_delay: std::time::Duration,
	pub start_group: Option<u64>,
	pub end_group: Option<u64>,
}

impl<'a> Message<'a> for SubscribeOk {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::Subscribe);
	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 => {
				w.u8(super::priority::to_wire(self.priority));
			}
			Version::Lite02 => {}
			// Lite05+ never sends SUBSCRIBE_OK, but keep the field layout matching
			// Lite03/04 so a stray future use stays well-formed.
			_ => {
				w.u8(super::priority::to_wire(self.priority));
				pad_group_order(w, version)?;
				w.varint(u64::try_from(self.max_delay.as_millis()).map_err(|_| EncodeError::BoundsExceeded)?)?;
				w.varint_opt(self.start_group)?;
				w.varint_opt(self.end_group)?;
			}
		}

		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 => Ok(Self {
				priority: super::priority::from_wire(r.u8()?),
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
				let priority = super::priority::from_wire(r.u8()?);
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

/// Resolves where a Lite05+ subscription starts. The first message the publisher sends,
/// once the start is known. A start above the requested one implicitly drops the groups
/// below it.
///
/// There is no start *frame*: a partial group is only served to a subscriber that asked
/// for one, so a range begins at its requested frame when it starts at its requested
/// group, and at frame 0 of a later one otherwise.
#[derive(Clone, Debug)]
pub struct SubscribeStart {
	/// The publisher's largest (group, frame) when it answered, `None` for a track with
	/// nothing yet. Lite07+ only; not on the wire before, where it decodes as `None`.
	pub largest: Option<crate::track::Position>,
	/// The group each range's delivery starts at, by index, `None` for a range with no
	/// group there. Before lite-07 the wire carries the one range's start as `Group`.
	pub starts: Vec<Option<u64>>,
}

impl<'a> Message<'a> for SubscribeStart {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::Subscribe);
	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_track_stream() {
			return Err(DecodeError::Version);
		}
		// Lite-07 added the largest position along with the ranges.
		if !version.has_ranges() {
			return Ok(Self {
				largest: None,
				starts: vec![Some(r.varint()?)],
			});
		}
		// Group + 1, so 0 is a track with nothing yet; the frame follows only otherwise.
		let largest = match r.varint_opt()? {
			Some(group) => Some(crate::track::Position {
				group,
				frame: r.varint()?,
			}),
			None => None,
		};
		// Not preallocated: each start costs a byte of the bounded message.
		let mut starts = Vec::new();
		for _ in 0..r.varint()? {
			starts.push(r.varint_opt()?);
		}
		Ok(Self { largest, starts })
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_track_stream() {
			return Err(EncodeError::Version);
		}
		if !version.has_ranges() {
			let [Some(group)] = self.starts[..] else {
				return Err(EncodeError::InvalidState);
			};
			return w.varint(group);
		}
		w.varint_opt(self.largest.map(|largest| largest.group))?;
		if let Some(largest) = self.largest {
			w.varint(largest.frame)?;
		}
		w.varint(self.starts.len() as u64)?;
		for start in &self.starts {
			w.varint_opt(*start)?;
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
	/// The error the track ended with, if it did not end cleanly. Lite-07 and later.
	pub error: Option<StreamError>,
}

impl<'a> Message<'a> for SubscribeEnd {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::Subscribe);
	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_track_stream() {
			return Err(DecodeError::Version);
		}
		let group = r.varint()?;
		let error = match version.has_end_error() {
			true => r
				.varint_opt()?
				.map(|code| u32::try_from(code).map_or(StreamError::Internal, |code| version.decode_stream_code(code))),
			false => None,
		};
		Ok(Self { group, error })
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_track_stream() {
			return Err(EncodeError::Version);
		}
		w.varint(self.group)?;
		match version.has_end_error() {
			true => w.varint_opt(self.error.as_ref().map(|err| version.encode_stream_code(err).into()))?,
			false if self.error.is_some() => return Err(EncodeError::Version),
			false => {}
		}
		Ok(())
	}
}

/// Sent by the subscriber to update subscription parameters.
///
/// Lite03+ only.
#[derive(Clone, Debug)]
pub struct SubscribeUpdate {
	/// A join requesting this group or later, judged against the local refresh ratio.
	pub wanted_group: Option<u64>,
	pub priority: i8,
	pub max_delay: std::time::Duration,
	/// The replacement for every range; see [`Subscribe::ranges`].
	pub ranges: Vec<Range>,
	/// See [`Subscribe::order`].
	pub order: Order,
}

impl<'a> Message<'a> for SubscribeUpdate {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::Subscribe);
	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {
				return Err(DecodeError::Version);
			}
			_ => {}
		}

		let priority = super::priority::from_wire(r.u8()?);
		let order = decode_order(r, version)?;
		let max_delay = std::time::Duration::from_millis(r.varint()?);
		let ranges = decode_ranges(r, version)?;

		let wanted_group = match version.has_refresh() {
			true => r.varint()?.checked_sub(1),
			false => None,
		};

		Ok(Self {
			wanted_group,
			priority,
			max_delay,
			ranges,
			order,
		})
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {
				return Err(EncodeError::Version);
			}
			_ => {}
		}

		w.u8(super::priority::to_wire(self.priority));
		encode_order(w, version, self.order)?;
		w.varint(u64::try_from(self.max_delay.as_millis()).map_err(|_| EncodeError::BoundsExceeded)?)?;
		encode_ranges(w, version, &self.ranges)?;

		if version.has_refresh() {
			w.varint_opt(self.wanted_group)?;
		}
		Ok(())
	}
}

/// Indicates that one or more groups have been dropped.
///
/// The range `[start, end]` is inclusive on both ends. For example,
/// `start = 5, end = 7` means groups 5, 6, and 7 were dropped.
///
/// Lite03+; publishers send it on Lite05 and later.
#[derive(Clone, Debug)]
pub struct SubscribeDrop {
	/// The first absolute group sequence in the dropped range.
	pub start: u64,

	/// The last absolute group sequence in the dropped range (inclusive).
	pub end: u64,

	/// Why the groups were given up: a stream error code on lite-07 and later, or an
	/// application-specific code before, where 0 means simply unavailable.
	pub error: u64,
}

#[cfg(feature = "async")]
impl SubscribeDrop {
	/// Name `groups` given up with `err`, as `version` carries it.
	pub fn new(groups: std::ops::RangeInclusive<u64>, err: &StreamError, version: Version) -> Self {
		Self {
			start: *groups.start(),
			end: *groups.end(),
			// Before lite-07 the code is the application's, so it says nothing of why.
			error: match version.has_drop_code() {
				true => version.encode_stream_code(err).into(),
				false => 0,
			},
		}
	}

	/// The error the dropped groups' readers end with, as `version` reads the code.
	///
	/// Before lite-07 the code is the application's, so every drop reads as
	/// [`StreamError::Old`]: the publisher gave the groups up, and a reader moves on to a
	/// newer one as it would past a superseded group.
	pub fn error(&self, version: Version) -> StreamError {
		match version.has_drop_code() {
			true => u32::try_from(self.error).map_or(StreamError::Internal, |code| version.decode_stream_code(code)),
			false => StreamError::Old,
		}
	}
}

impl<'a> Message<'a> for SubscribeDrop {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::Subscribe);
	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {
				return Err(DecodeError::Version);
			}
			_ => {}
		}

		let msg = Self {
			start: r.varint()?,
			end: r.varint()?,
			error: r.varint()?,
		};
		if msg.start.saturating_sub(msg.end) > 1 {
			return Err(DecodeError::InvalidValue);
		}
		Ok(msg)
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {
				return Err(EncodeError::Version);
			}
			_ => {}
		}

		if self.start.saturating_sub(self.end) > 1 {
			return Err(EncodeError::InvalidState);
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
/// - Lite05+: `0x0` SUBSCRIBE_START, `0x1` SUBSCRIBE_END, `0x2` SUBSCRIBE_DROP
///   (SUBSCRIBE_OK was removed; acceptance is implicit).
#[derive(Clone, Debug)]
pub enum SubscribeResponse {
	Ok(SubscribeOk),
	Start(SubscribeStart),
	End(SubscribeEnd),
	Drop(SubscribeDrop),
}

/// Write a `type` varint followed by the size-prefixed message body.
fn encode_typed<'m, M: Message<'m>>(
	w: &mut Encoder<'_>,
	typ: u64,
	msg: &M,
	version: Version,
) -> Result<(), EncodeError> {
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
				Self::Drop(drop) => encode_typed(w, 2, drop, version)?,
				Self::Ok(_) => return Err(EncodeError::Version),
			},
		}

		Ok(())
	}
}

impl<'a> Decode<'a, Version> for SubscribeResponse {
	fn decode(buf: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
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
					2 => Ok(Self::Drop(SubscribeDrop::decode(buf, version)?)),
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
	fn drop_ranges_refuse_reversed_and_allow_empty_or_maximum_ends() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			assert!(matches!(
				SubscribeDrop::decode_slice(&[3, 3, 1, 0], version),
				Err(DecodeError::InvalidValue)
			));
			assert!(
				SubscribeDrop {
					start: 3,
					end: 1,
					error: 0
				}
				.encode_bytes(version)
				.is_err()
			);
			let max = if version == Version::Lite07 {
				u64::MAX
			} else {
				(1 << 62) - 1
			};
			for (start, end) in [(2, 1), (max, max)] {
				let msg = SubscribeDrop { start, end, error: 0 };
				let data = msg.encode_bytes(version).unwrap();
				let (decoded, _) = SubscribeDrop::decode_slice(&data, version).unwrap();
				assert_eq!((decoded.start, decoded.end), (start, end));
			}
		}
	}

	#[test]
	fn subscribe_start_roundtrips_on_lite05() {
		let resp = SubscribeResponse::Start(SubscribeStart {
			largest: None,
			starts: vec![Some(42)],
		});
		let mut buf = Vec::new();
		resp.encode(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05)
			.unwrap();
		let mut slice = buf.as_slice();
		match crate::coding::decode_buf(&mut slice, Version::Lite05, |r, v| SubscribeResponse::decode(r, v)).unwrap() {
			SubscribeResponse::Start(start) => assert_eq!(start.starts, [Some(42)]),
			other => panic!("expected Start, got {other:?}"),
		}
	}

	/// Lite-07 carries the publisher's largest position; earlier versions leave it off the
	/// wire, so it decodes as `None` there.
	#[test]
	fn subscribe_start_carries_the_largest_position_on_lite07() {
		for largest in [None, Some(crate::track::Position { group: 3, frame: 2 })] {
			let resp = SubscribeResponse::Start(SubscribeStart {
				largest,
				starts: vec![Some(4)],
			});
			let buf = resp.encode_bytes(Version::Lite07).unwrap();
			match SubscribeResponse::decode_slice(&buf, Version::Lite07).unwrap().0 {
				SubscribeResponse::Start(start) => assert_eq!(start.largest, largest),
				other => panic!("expected Start, got {other:?}"),
			}
		}
		let resp = SubscribeResponse::Start(SubscribeStart {
			largest: Some(crate::track::Position { group: 3, frame: 2 }),
			starts: vec![Some(4)],
		});
		let buf = resp.encode_bytes(Version::Lite06).unwrap();
		assert_eq!(buf.as_ref(), [0, 1, 4], "lite-06 has no largest position");
	}

	/// Lite-07 says where each range starts in place of the one `Group` earlier versions
	/// carry, which can name only a single range.
	#[test]
	fn subscribe_start_carries_the_range_starts_on_lite07() {
		let start = SubscribeStart {
			largest: None,
			starts: vec![Some(2), None, Some(0)],
		};
		let resp = SubscribeResponse::Start(start.clone());
		let buf = resp.encode_bytes(Version::Lite07).unwrap();
		assert_eq!(buf.as_ref(), [0, 5, 0, 3, 3, 0, 1]);
		match SubscribeResponse::decode_slice(&buf, Version::Lite07).unwrap().0 {
			SubscribeResponse::Start(decoded) => assert_eq!(decoded.starts, start.starts),
			other => panic!("expected Start, got {other:?}"),
		}
		assert!(resp.encode_bytes(Version::Lite06).is_err());
	}

	#[test]
	fn subscribe_end_roundtrips_on_lite05() {
		let resp = SubscribeResponse::End(SubscribeEnd { group: 7, error: None });
		let mut buf = Vec::new();
		resp.encode(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05)
			.unwrap();
		// Type, length, exclusive final group.
		assert_eq!(buf, [1, 1, 7]);
		let mut slice = buf.as_slice();
		match crate::coding::decode_buf(&mut slice, Version::Lite05, |r, v| SubscribeResponse::decode(r, v)).unwrap() {
			SubscribeResponse::End(end) => assert_eq!(end.group, 7),
			other => panic!("expected End, got {other:?}"),
		}
	}

	#[test]
	fn subscribe_end_carries_its_error_on_lite07() {
		let encode = |error: Option<StreamError>| {
			let resp = SubscribeResponse::End(SubscribeEnd { group: 7, error });
			let mut buf = Vec::new();
			resp.encode(&mut Encoder::new(&mut buf, Version::Lite07.into()), Version::Lite07)
				.unwrap();
			buf
		};
		let decode = |buf: &[u8]| {
			let mut slice = buf;
			match crate::coding::decode_buf(&mut slice, Version::Lite07, |r, v| SubscribeResponse::decode(r, v))
				.unwrap()
			{
				SubscribeResponse::End(end) => (end.group, end.error),
				other => panic!("expected End, got {other:?}"),
			}
		};
		// Type, length, exclusive final group, then the error code plus one, 0 for a clean end.
		let clean = encode(None);
		assert_eq!(clean, [1, 2, 7, 0]);
		assert_eq!(decode(&clean), (7, None));
		let errored = encode(Some(StreamError::App(7)));
		assert_eq!(errored, [1, 2, 7, 64 + 7 + 1]);
		assert_eq!(decode(&errored), (7, Some(StreamError::App(7))));

		// An older version has nowhere to carry it.
		let resp = SubscribeResponse::End(SubscribeEnd {
			group: 7,
			error: Some(StreamError::App(7)),
		});
		assert!(resp.encode_bytes(Version::Lite06).is_err());
	}

	#[test]
	fn subscribe_drop_is_restored_on_lite07() {
		let resp = SubscribeResponse::Drop(SubscribeDrop {
			start: 1,
			end: 3,
			error: 7,
		});
		let mut buf = Vec::new();
		resp.encode(&mut Encoder::new(&mut buf, Version::Lite07.into()), Version::Lite07)
			.unwrap();
		assert_eq!(buf, [2, 3, 1, 3, 7]);
		match crate::coding::decode_buf(&mut buf.as_slice(), Version::Lite07, |r, v| {
			SubscribeResponse::decode(r, v)
		})
		.unwrap()
		{
			SubscribeResponse::Drop(drop) => assert_eq!((drop.start, drop.end, drop.error), (1, 3, 7)),
			other => panic!("expected Drop, got {other:?}"),
		}
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
		match crate::coding::decode_buf(&mut slice, Version::Lite05, |r, v| SubscribeResponse::decode(r, v)).unwrap() {
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

	fn absolute(group: u64, frame: u64) -> Option<Anchor> {
		Some(Anchor::Absolute(Position { group, frame }))
	}

	fn subscribe_sample() -> Subscribe<'static> {
		Subscribe {
			order: crate::track::Order::Descending,
			epoch: None,
			id: 1,
			broadcast: Path::from_static("room"),
			track: Cow::Borrowed("video"),
			priority: 3,
			max_delay: std::time::Duration::from_millis(250),
			// Group 7 from frame 4, through frame 2 of group 9.
			ranges: vec![Range {
				start: absolute(7, 4),
				end: absolute(9, 3),
			}],
		}
	}

	fn roundtrip(msg: &Subscribe<'_>, version: Version) -> Subscribe<'static> {
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		crate::coding::decode_from(&mut buf.as_slice(), version, Subscribe::decode_msg)
			.unwrap()
			.into_owned()
	}

	fn encode(msg: &Subscribe<'_>, version: Version) -> Result<Vec<u8>, EncodeError> {
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)?;
		Ok(buf)
	}

	/// Lite-06 carries one range's frame bounds; an inclusive `Frame End` on the wire is the
	/// exclusive position just past it here.
	#[test]
	fn subscribe_frame_bounds_roundtrip() {
		let msg = subscribe_sample();
		assert_eq!(roundtrip(&msg, Version::Lite06).ranges, msg.ranges);
	}

	/// The whole-group defaults are what a version without the fields decodes to, so
	/// lite-05 stays byte-identical. Compared without a floor, since `Group Start` itself
	/// encodes differently across the two (see `group_start_is_absolute_on_lite06`).
	#[test]
	fn subscribe_drops_the_retired_ordered_byte_on_lite06() {
		let mut msg = subscribe_sample();
		msg.ranges = vec![Range {
			start: None,
			end: absolute(10, 0),
		}];

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

		let got = crate::coding::decode_from(&mut lite05.as_slice(), Version::Lite05, |r, v| {
			Subscribe::decode_msg(r, v)
		})
		.unwrap();
		assert_eq!(got.ranges, msg.ranges);
	}

	/// Lite06 carries the raw floor; pre-06 wires encode the sequence + 1 with 0 meaning
	/// the latest group. An explicit group 0 is that sequence, so it round-trips as group 0
	/// rather than as the latest group.
	#[test]
	fn group_start_is_absolute_on_lite06() {
		let mut msg = subscribe_sample();
		msg.ranges = vec![Range {
			start: absolute(7, 0),
			end: None,
		}];

		let lite05 = encode(&msg, Version::Lite05).unwrap();
		let lite06 = encode(&msg, Version::Lite06).unwrap();
		assert_eq!(roundtrip(&msg, Version::Lite05).ranges, msg.ranges);
		assert_eq!(roundtrip(&msg, Version::Lite06).ranges, msg.ranges);
		// The raw byte differs: 7 on the wire, not 7 + 1.
		assert_ne!(lite05, lite06);

		// No floor and a floor of 0 are the same absence of a constraint on lite-06:
		// byte-identical on the wire, and decoded as no start.
		msg.ranges = vec![Range::OLDEST];
		let absent = encode(&msg, Version::Lite06).unwrap();
		msg.ranges[0].start = absolute(0, 0);
		let zero = encode(&msg, Version::Lite06).unwrap();
		assert_eq!(absent, zero);
		let got = crate::coding::decode_from(&mut zero.as_slice(), Version::Lite06, |r, v| {
			Subscribe::decode_msg(r, v)
		})
		.unwrap();
		assert_eq!(got.ranges, [Range::OLDEST]);

		// On the pre-06 wire an explicit group 0 is sequence + 1, distinct from an
		// absent start (the latest group).
		let explicit = encode(&msg, Version::Lite05).unwrap();
		assert_eq!(roundtrip(&msg, Version::Lite05).ranges, msg.ranges);
		msg.ranges = vec![Range::OLDEST];
		let latest = encode(&msg, Version::Lite05).unwrap();
		assert_ne!(explicit, latest);
		assert_eq!(roundtrip(&msg, Version::Lite05).ranges, [Range::OLDEST]);

		// A live start is the same absent `Group Start` on both.
		let mut live = msg.clone();
		live.ranges[0].start = Some(Anchor::Relative(0));
		assert_eq!(encode(&live, Version::Lite05).unwrap(), latest);
		assert_eq!(encode(&live, Version::Lite06).unwrap(), absent);
	}

	/// A subscription can resume partway through group 0 (a catalog never leaves it), so
	/// a `Frame Start` qualifying the zero floor must survive the round trip.
	#[test]
	fn frame_start_may_qualify_group_zero_on_lite06() {
		let mut msg = subscribe_sample();
		msg.ranges[0].start = absolute(0, 4);
		assert_eq!(roundtrip(&msg, Version::Lite06).ranges, msg.ranges);
	}

	/// Silently widening to the whole group would deliver frames the caller excluded.
	#[test]
	fn subscribe_frame_bounds_rejected_before_lite06() {
		assert!(encode(&subscribe_sample(), Version::Lite05).is_err());
	}

	/// Frames are numbered per group, so a lite-06 `Frame End` without the `Group End` it
	/// qualifies has nothing to count from.
	#[test]
	fn subscribe_frame_end_without_group_end_is_invalid() {
		let version = Version::Lite06;
		let mut buf = Vec::new();
		let mut w = Encoder::new(&mut buf, version.into());
		w.varint(1).unwrap();
		Path::from_static("room").encode(&mut w, version).unwrap();
		w.string("video").unwrap();
		w.u8(0);
		w.varint(0).unwrap(); // max delay
		w.varint(0).unwrap(); // group start
		w.varint(0).unwrap(); // group end: none
		w.varint(0).unwrap(); // frame start
		w.varint(8).unwrap(); // frame end: 7
		assert!(matches!(
			crate::coding::decode_from(&mut buf.as_slice(), version, Subscribe::decode_msg),
			Err(DecodeError::InvalidSubscribeLocation)
		));
	}

	/// Lite-07 carries every range, each end absolute, relative, at a time, or open.
	#[test]
	fn ranges_roundtrip_on_lite07() {
		let mut msg = subscribe_sample();
		msg.ranges = vec![
			Range::OLDEST,
			Range {
				start: absolute(7, 4),
				end: absolute(9, 3),
			},
			Range {
				start: Some(Anchor::Relative(2)),
				end: Some(Anchor::Relative(0)),
			},
			Range {
				start: Some(Anchor::Time(1_500)),
				end: Some(Anchor::Time(3_500)),
			},
			Range {
				start: absolute(0, 0),
				end: None,
			},
		];
		assert_eq!(roundtrip(&msg, Version::Lite07).ranges, msg.ranges);

		let update = SubscribeUpdate {
			wanted_group: None,
			priority: 3,
			max_delay: std::time::Duration::from_millis(250),
			ranges: msg.ranges.clone(),
			order: crate::track::Order::Ascending,
		};
		let buf = update.encode_bytes(Version::Lite07).unwrap();
		let got = crate::coding::decode_buf(&mut buf.as_ref(), Version::Lite07, |r, v| SubscribeUpdate::decode(r, v))
			.unwrap();
		assert_eq!(got.ranges, msg.ranges);
	}

	/// A subscription asking for nothing is refused both ways, and an unknown anchor type
	/// is malformed.
	#[test]
	fn lite07_refuses_an_empty_list_and_unknown_anchors() {
		let version = Version::Lite07;
		let mut msg = subscribe_sample();
		msg.ranges.clear();
		assert!(matches!(encode(&msg, version), Err(EncodeError::InvalidState)));

		msg.ranges = vec![Range::OLDEST];
		let mut buf = encode(&msg, version).unwrap();
		// The list is last: a count of one, then two anchors of type 0.
		let at = buf.len() - 3;
		assert_eq!(&buf[at..], [1, 0, 0]);
		buf[at] = 0;
		buf.truncate(at + 1);
		assert!(matches!(
			crate::coding::decode_from(&mut buf.as_slice(), version, Subscribe::decode_msg),
			Err(DecodeError::InvalidSubscribeLocation)
		));

		let mut buf = encode(&msg, version).unwrap();
		let last = buf.len() - 1;
		buf[last] = 4;
		assert!(matches!(
			crate::coding::decode_from(&mut buf.as_slice(), version, Subscribe::decode_msg),
			Err(DecodeError::InvalidValue)
		));
	}

	/// An older wire carries one span with absolute ends, so anything else is refused for
	/// the caller to narrow first.
	#[test]
	fn older_wires_refuse_what_one_span_cannot_carry() {
		let refused = [
			vec![Range::OLDEST, Range::OLDEST],
			vec![Range {
				start: Some(Anchor::Relative(2)),
				end: None,
			}],
			vec![Range {
				start: Some(Anchor::Time(1_500)),
				end: None,
			}],
			vec![Range {
				start: None,
				end: Some(Anchor::Relative(1)),
			}],
		];
		for version in [Version::Lite03, Version::Lite05, Version::Lite06] {
			for ranges in &refused {
				let mut msg = subscribe_sample();
				msg.ranges = ranges.clone();
				assert!(
					matches!(encode(&msg, version), Err(EncodeError::Version)),
					"{version}: {ranges:?}"
				);
			}
		}
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

	/// Lite-07 carries `Wanted Group` on SUBSCRIBE_UPDATE; older versions drop it.
	#[test]
	fn wanted_group_roundtrips_only_on_lite07() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			for wanted in [None, Some(0), Some(1), Some(u64::MAX - 1)] {
				let update = SubscribeUpdate {
					wanted_group: wanted,
					priority: 0,
					max_delay: std::time::Duration::ZERO,
					ranges: vec![Range::OLDEST],
					order: crate::track::Order::Descending,
				};
				let buf = update.encode_bytes(version).unwrap();
				let got = crate::coding::decode_buf(&mut buf.as_ref(), version, |r, v| SubscribeUpdate::decode(r, v))
					.unwrap();
				assert_eq!(got.wanted_group, if version.has_refresh() { wanted } else { None });
			}
		}
	}

	/// Lite-07 carries `Group Order` right after the priority, on SUBSCRIBE and
	/// SUBSCRIBE_UPDATE alike, and refuses a value it does not define.
	#[test]
	fn group_order_roundtrips_on_lite07() {
		use crate::track::Order;

		let version = Version::Lite07;
		for order in [Order::Ascending, Order::Descending] {
			let mut msg = subscribe_sample();
			msg.order = order;
			assert_eq!(roundtrip(&msg, version).order, order);

			let update = SubscribeUpdate {
				wanted_group: None,
				priority: 3,
				max_delay: std::time::Duration::from_millis(250),
				ranges: vec![Range::OLDEST],
				order,
			};
			let mut buf = Vec::new();
			update
				.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)
				.unwrap();
			// The priority, then the order.
			assert_eq!(buf[1], u8::from(order == Order::Ascending));
			let got = crate::coding::decode_buf(&mut buf.as_slice(), version, |r, v| SubscribeUpdate::decode_msg(r, v))
				.unwrap();
			assert_eq!(got.order, order);

			buf[1] = 2;
			assert!(matches!(
				crate::coding::decode_buf(&mut buf.as_slice(), version, |r, v| SubscribeUpdate::decode_msg(r, v)),
				Err(DecodeError::InvalidValue)
			));
		}
	}

	/// No earlier wire has `Group Order`, so an ascending subscription falls back to
	/// descending, byte-identical to one that never asked.
	#[test]
	fn group_order_falls_back_before_lite07() {
		use crate::track::Order;

		for version in [Version::Lite03, Version::Lite05, Version::Lite06] {
			let mut msg = subscribe_sample();
			msg.ranges = vec![Range {
				start: absolute(7, 0),
				end: absolute(10, 0),
			}];
			let descending = encode(&msg, version).unwrap();
			msg.order = Order::Ascending;
			let ascending = encode(&msg, version).unwrap();
			assert_eq!(ascending, descending, "{version}");

			let got = crate::coding::decode_from(&mut ascending.as_slice(), version, Subscribe::decode_msg).unwrap();
			assert_eq!(got.order, Order::Descending, "{version}");
		}
	}
}
