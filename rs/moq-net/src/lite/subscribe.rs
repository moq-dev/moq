use std::borrow::Cow;

use crate::{
	Path, StreamError,
	coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder, Form, StreamCodes, varint},
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
	/// The identity the subscriber expects; see [`crate::origin::RouteInfo::identity`].
	/// Lite07+ only.
	pub identity: Option<crate::Identity>,
	/// The forwarding node whose claim the request follows, or `None` for whichever the
	/// publisher selects. Lite07+ only.
	pub node: Option<crate::route::NodeId>,
	pub track: Cow<'a, str>,
	pub priority: i8,
	/// The subscriber's `Subscriber Max Age` before lite-07, or `None` for no limit beyond the
	/// publisher's. Lite01/02 carry none and decode as `None`; lite03-06 can't say `None` (see
	/// [`encode_max_delay`]). Lite-07 carries a max age on each [`Anchor::Live`] start instead,
	/// so this is ignored there and decodes as `None`.
	pub max_delay: Option<std::time::Duration>,
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
			identity: self.identity,
			node: self.node,
			track: Cow::Owned(self.track.into_owned()),
			priority: self.priority,
			max_delay: self.max_delay,
			ranges: self.ranges,
			order: self.order,
		}
	}
}

pub(crate) use crate::track::Policy;

/// A lite-07 SUBSCRIBE_UPDATE's change to a live range in place, which keeps where it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retune {
	/// Its new priority and order.
	pub policy: Policy,
	/// A live join's new max age, `None` for no limit; always `None` for any other range.
	pub max: Option<std::time::Duration>,
}

#[cfg(test)]
impl Retune {
	/// A retune of a live join's max age alone, at the default priority and order.
	pub(crate) fn max(max: Option<std::time::Duration>) -> Self {
		Self {
			policy: Policy::default(),
			max,
		}
	}
}

/// One end of a [`Range`], as the wire carries it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Anchor {
	/// A position: a start delivers from it, and an end stops just before it. Matches the
	/// named group exactly (never falling back to an earlier group whose span contains it).
	/// Frame may be 0 (whole group from frame 0, or exclusive end before that group's frame 0).
	Absolute(crate::track::Position),
	/// The group containing this time in the track's timescale (the highest start at or
	/// below it), included as an end. Lite07+ only.
	Time(u64),
	/// The group containing the live edge minus `min` (`None` for no lag limit, reaching back
	/// past every group), whose groups expire once they fall `max` behind the live edge (`None`
	/// for no limit beyond the publisher's). Both are carried in whole milliseconds, rounded up.
	/// A start only, lite07+ only: an older wire carries it as no start.
	Live {
		min: Option<std::time::Duration>,
		max: Option<std::time::Duration>,
	},
	/// A live join already placed at this position, whose groups expire once they fall `max`
	/// behind the live edge, as a [`Self::Live`] start's do. Its frame may be 0, the whole
	/// group. A start only, lite07+ only: a route switch asking a new upstream for a join an
	/// earlier one placed, which an older wire carries as the position.
	Joined {
		at: crate::track::Position,
		max: Option<std::time::Duration>,
	},
}

/// A span of a track, as the wire carries it.
///
/// Before lite-07 a subscription carries exactly one, in `Group Start` and `Group End` and
/// their frame bounds. An absent `Group Start` decodes as no start, and so does a lite-06
/// `Group Start` of 0 with a `Frame Start` of 0, the same absence of a floor there. No start
/// encodes as an absent `Group Start`, which a pre-06 publisher reads as the latest group and
/// a lite-06 one as no floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Range {
	/// Where delivery starts, or `None` for the oldest group the max age allows.
	pub start: Option<Anchor>,
	/// Where delivery stops, or `None` to follow the live edge.
	pub end: Option<Anchor>,
	/// How urgently this range's groups go out, higher first. Lite07+ carries one per range;
	/// earlier wires carry [`Subscribe::priority`] for the whole subscription instead.
	pub priority: i8,
	/// Which of this range's pending groups goes out first. Lite07+ carries one per range;
	/// earlier wires carry [`Subscribe::order`] for the whole subscription instead.
	pub order: Order,
}

impl Range {
	/// Everything from the oldest group the max age allows onward, at the default priority and
	/// order.
	pub const OPEN: Self = Self {
		start: None,
		end: None,
		priority: 0,
		order: Order::Descending,
	};

	/// This range's span at `policy`'s priority and order.
	pub(crate) fn with_policy(self, policy: Policy) -> Self {
		Self {
			priority: policy.priority,
			order: policy.order,
			..self
		}
	}

	/// This range's priority and order.
	pub(crate) fn policy(&self) -> Policy {
		Policy {
			priority: self.priority,
			order: self.order,
		}
	}

	/// Whether `other` asks for the same span, whatever its policy and live join's max age:
	/// the edit a SUBSCRIBE_UPDATE retunes in place rather than placing afresh.
	fn retunes(&self, other: &Self) -> bool {
		let start = match (self.start, other.start) {
			(Some(Anchor::Live { min, .. }), Some(Anchor::Live { min: other, .. })) => min == other,
			(Some(Anchor::Joined { at, .. }), Some(Anchor::Joined { at: other, .. })) => at == other,
			(start, other) => start == other,
		};
		start && self.end == other.end
	}

	/// This range's live join max age, `None` for one that is not a live join.
	fn max(&self) -> Option<Option<std::time::Duration>> {
		match self.start {
			Some(Anchor::Live { max, .. } | Anchor::Joined { max, .. }) => Some(max),
			_ => None,
		}
	}

	/// Whether the range follows the live edge, starting at it or never ending, so its
	/// SUBSCRIBE_START always carries a Largest.
	pub(crate) fn follows_live(&self) -> bool {
		matches!(self.start, Some(Anchor::Live { .. })) || self.end.is_none()
	}

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
			..Self::OPEN
		}
	}
}

impl<'a> Message<'a> for Subscribe<'a> {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::Subscribe);
	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let id = r.varint()?;
		let broadcast = Path::decode(r, version)?;
		let identity = super::identity::decode_pin(r, version)?;
		let node = super::identity::decode_node(r, version)?;
		let track = Cow::Borrowed(r.str()?);

		// An older wire's one priority and order apply to its one range.
		let (priority, order, max_delay, ranges) = match version {
			Version::Lite01 | Version::Lite02 => {
				let priority = super::priority::from_wire(r.u8()?);
				let range = Range::OPEN.with_policy(Policy {
					priority,
					order: Order::Descending,
				});
				(priority, Order::Descending, None, vec![range])
			}
			// Each range carries its own priority and order.
			_ if version.has_ranges() => {
				let ranges = decode_ranges(r, version)?;
				let policy = Policy::top(ranges.iter().map(Range::policy));
				(policy.priority, policy.order, None, ranges)
			}
			_ => {
				let priority = super::priority::from_wire(r.u8()?);
				let order = decode_order(r, version)?;
				let max_delay = decode_max_delay(r, version)?;
				let ranges = decode_ranges(r, version)?;
				let ranges = stamped(ranges, Policy { priority, order });
				(priority, order, max_delay, ranges)
			}
		};
		Ok(Self {
			id,
			broadcast,
			identity,
			node,
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
		super::identity::encode_pin(w, version, self.identity.as_ref())?;
		super::identity::encode_node(w, version, self.node)?;
		w.string(&self.track)?;

		match version {
			// No field carries a range: the publisher serves the live edge.
			Version::Lite01 | Version::Lite02 => w.u8(super::priority::to_wire(self.priority)),
			// Each range carries its own priority and order.
			_ if version.has_ranges() => encode_ranges(w, version, &self.ranges)?,
			_ => {
				w.u8(super::priority::to_wire(self.priority));
				encode_order(w, version, self.order)?;
				encode_max_delay(w, version, self.max_delay)?;
				encode_ranges(w, version, &self.ranges)?;
			}
		}

		Ok(())
	}
}

/// `ranges` at `policy`, the one an older wire carries for the whole subscription.
fn stamped(ranges: Vec<Range>, policy: Policy) -> Vec<Range> {
	ranges.into_iter().map(|range| range.with_policy(policy)).collect()
}

/// What lite03-06 send for no `Subscriber Max Age` limit, which their bare milliseconds can't
/// say: the largest every implementation carries losslessly.
const MAX_SAFE_DELAY_MS: u64 = (1 << 53) - 1;

/// Decode the `Subscriber Max Age`, bare milliseconds before lite-07, where
/// [`MAX_SAFE_DELAY_MS`] or more says no limit. Lite-07 carries none: each live start has
/// its own.
fn decode_max_delay(r: &mut Decoder<'_>, version: Version) -> Result<Option<std::time::Duration>, DecodeError> {
	if version.has_ranges() {
		return Ok(None);
	}
	let millis = r.varint()?;
	Ok((millis < MAX_SAFE_DELAY_MS).then(|| std::time::Duration::from_millis(millis)))
}

/// Encode the inverse of [`decode_max_delay`]: no limit goes out as [`MAX_SAFE_DELAY_MS`].
fn encode_max_delay(
	w: &mut Encoder<'_>,
	version: Version,
	max_delay: Option<std::time::Duration>,
) -> Result<(), EncodeError> {
	if version.has_ranges() {
		return Ok(());
	}
	let millis = max_delay
		.map(|max| u64::try_from(max.as_millis()).map_err(|_| EncodeError::BoundsExceeded))
		.transpose()?;
	w.varint(millis.unwrap_or(MAX_SAFE_DELAY_MS))
}

/// Decode the ranges SUBSCRIBE and SUBSCRIBE_UPDATE carry after the max age.
///
/// Lite-07 carries a non-empty list. Older wires carry one span in `Group Start`,
/// `Group End`, and their frame bounds; see [`Range`].
fn decode_ranges(r: &mut Decoder<'_>, version: Version) -> Result<Vec<Range>, DecodeError> {
	if version.has_ranges() {
		let ranges = decode_range_list(r)?;
		// A subscription that asks for nothing has no use: refuse it rather than park it.
		if ranges.is_empty() {
			return Err(DecodeError::InvalidSubscribeLocation);
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
	Ok(vec![Range {
		start,
		end,
		..Range::OPEN
	}])
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
		return encode_range_list(w, ranges);
	}

	let [range] = ranges else {
		return Err(EncodeError::Version);
	};
	let (start_group, start_frame) = match range.start {
		// A live join can't be carried, so it goes out as no start.
		None | Some(Anchor::Live { .. }) => (None, 0),
		Some(Anchor::Absolute(start)) => (Some(start.group), start.frame),
		Some(Anchor::Time(_) | Anchor::Joined { .. }) => return Err(EncodeError::Version),
	};
	let (end_group, end_frame) = match range.end {
		None => (None, None),
		// The wire's end is inclusive: the group before a head-of-group end, or the frame
		// before a mid-group one. Nothing comes before the very first position.
		Some(Anchor::Absolute(end)) => match end.frame.checked_sub(1) {
			Some(frame) => (Some(end.group), Some(frame)),
			None => (Some(end.group.checked_sub(1).ok_or(EncodeError::InvalidState)?), None),
		},
		Some(Anchor::Time(_)) => return Err(EncodeError::Version),
		Some(Anchor::Live { .. } | Anchor::Joined { .. }) => return Err(EncodeError::InvalidState),
	};
	encode_start_group(w, version, start_group)?;
	w.varint_opt(end_group)?;
	encode_frame_bounds(w, version, start_group, start_frame, end_group, end_frame)
}

/// Decode a lite-07 `Range Count` and its ranges, which may be none.
fn decode_range_list(r: &mut Decoder<'_>) -> Result<Vec<Range>, DecodeError> {
	let count = r.varint()?;
	// Not preallocated: each range costs at least two bytes of the bounded message, so a
	// count the bytes can't back runs out before it costs anything.
	let mut ranges = Vec::new();
	for _ in 0..count {
		let policy = decode_policy(r)?;
		let start = decode_anchor(r, false)?;
		let end = decode_anchor(r, true)?;
		ranges.push(
			Range {
				start,
				end,
				..Range::OPEN
			}
			.with_policy(policy),
		);
	}
	Ok(ranges)
}

/// Decode a lite-07 range's or retune's `Priority` then `Group Order`.
fn decode_policy(r: &mut Decoder<'_>) -> Result<Policy, DecodeError> {
	let priority = super::priority::from_wire(r.u8()?);
	let order = decode_order(r, Version::Lite07)?;
	Ok(Policy { priority, order })
}

/// Encode the inverse of [`decode_policy`].
fn encode_policy(w: &mut Encoder<'_>, policy: Policy) -> Result<(), EncodeError> {
	w.u8(super::priority::to_wire(policy.priority));
	encode_order(w, Version::Lite07, policy.order)
}

/// Encode the inverse of [`decode_range_list`].
fn encode_range_list(w: &mut Encoder<'_>, ranges: &[Range]) -> Result<(), EncodeError> {
	w.varint(ranges.len() as u64)?;
	for range in ranges {
		encode_policy(w, range.policy())?;
		for (anchor, end) in [(range.start, false), (range.end, true)] {
			let (kind, values) = wire_anchor(anchor, end)?;
			w.varint(kind)?;
			for value in values.iter().flatten() {
				w.varint(*value)?;
			}
		}
	}
	Ok(())
}

/// The bytes a lite-07 range takes on the wire, or `None` when it can't go out.
pub(super) fn range_size(range: &Range, form: Form) -> Option<usize> {
	let size = |value| varint::size(value, form).ok();
	let anchor = |anchor, end| -> Option<usize> {
		let (kind, values) = wire_anchor(anchor, end).ok()?;
		values
			.iter()
			.flatten()
			.try_fold(size(kind)?, |total, value| Some(total + size(*value)?))
	};
	// Its priority and order, a byte each.
	Some(2 + anchor(range.start, false)? + anchor(range.end, true)?)
}

/// Decode one lite-07 range end: its type, then the fields that type carries. Type 0 is no
/// anchor, a start the max age places and the live edge as an end. Type 1 is a presentation
/// time (containment lookup, inclusive as an end). Type 2 is a position (exact group match,
/// exclusive as an end), whose frame may be 0. Type 3, a live join's lag then its max age,
/// each in milliseconds plus one with zero for no limit, is a start only, so `end` refuses it.
/// So is type 4, a live join already placed: its group's start, a frame that may be 0, then
/// its max age as type 3 carries it.
fn decode_anchor(r: &mut Decoder<'_>, end: bool) -> Result<Option<Anchor>, DecodeError> {
	Ok(match r.varint()? {
		0 => None,
		1 => Some(Anchor::Time(r.varint()?)),
		2 => {
			let group = r.varint()?;
			let frame = r.varint()?;
			Some(Anchor::Absolute(crate::track::Position { group, frame }))
		}
		3 if !end => Some(Anchor::Live {
			min: r.varint_opt()?.map(std::time::Duration::from_millis),
			max: r.varint_opt()?.map(std::time::Duration::from_millis),
		}),
		4 if !end => Some(Anchor::Joined {
			at: crate::track::Position {
				group: r.varint()?,
				frame: r.varint()?,
			},
			max: r.varint_opt()?.map(std::time::Duration::from_millis),
		}),
		_ => return Err(DecodeError::InvalidValue),
	})
}

/// The lite-07 type and values of a range's start or `end`, the inverse of [`decode_anchor`].
///
/// An absolute position always goes out as type 2 (its group's start then its frame, which
/// may be 0): a start from that exact group/frame, an exclusive end just before it. A time
/// anchor goes out as type 1 (containment lookup, inclusive as an end). A live join goes out
/// as its lag then its max age, each in milliseconds rounded up, plus one so zero says no
/// limit, and one already placed as its position then its max age, each only as a start.
fn wire_anchor(anchor: Option<Anchor>, end: bool) -> Result<(u64, [Option<u64>; 3]), EncodeError> {
	let millis = wire_millis;
	Ok(match anchor {
		None => (0, [None, None, None]),
		Some(Anchor::Time(time)) => (1, [Some(time), None, None]),
		Some(Anchor::Absolute(position)) => (2, [Some(position.group), Some(position.frame), None]),
		Some(Anchor::Live { .. } | Anchor::Joined { .. }) if end => return Err(EncodeError::InvalidState),
		Some(Anchor::Live { min, max }) => (3, [Some(millis(min)), Some(millis(max)), None]),
		Some(Anchor::Joined { at, max }) => (4, [Some(at.group), Some(at.frame), Some(millis(max))]),
	})
}

/// A lite-07 age limit in whole milliseconds, rounded up, plus one so zero says no limit.
fn wire_millis(limit: Option<std::time::Duration>) -> u64 {
	limit.map_or(0, |limit| crate::track::live_millis(limit).saturating_add(1))
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
/// `Some(0)`. Pre-06 wires encode the key + 1, with 0 meaning the latest group
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
/// `Some(0)` are the same group 0. A pre-06 wire encodes the key + 1: `None` is 0
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

/// SUBSCRIBE_START: where a range's delivery starts, once the publisher resolves it, and on
/// lite-07 what it knows of the track's largest position. Lite-07 answers each range on its
/// own, the SUBSCRIBE's and every one an update adds; before it the one start answers the
/// SUBSCRIBE's span, as the first message. A start above the requested one implicitly
/// drops the groups below it.
///
/// There is no start *frame*: a partial group is only served to a subscriber that asked
/// for one, so a range begins at its requested frame when it starts at its requested
/// group, and at frame 0 of a later one otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscribeStart {
	/// The range's Range ID. Before lite-07 always 0, the one range.
	pub id: u64,
	/// The group its delivery starts at, `None` for a range with no group there. Before
	/// lite-07 always `Some`, the wire's `Group`.
	pub start: Option<u64>,
	/// What the publisher knows of the track's largest position. Lite-07 only; not on the
	/// wire before, where it decodes as [`Edge::Unknown`].
	pub largest: Edge,
}

/// What a SUBSCRIBE_START says of the publisher's largest position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
	/// Not known, as when a relay answers a bounded range from its cache.
	Unknown,
	/// The track has no group yet.
	Empty,
	/// The publisher's largest position.
	Largest(Largest),
}

/// The publisher's largest position, as SUBSCRIBE_START reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Largest {
	/// The newest group and its last frame so far.
	pub position: crate::track::Position,
	/// The group ended at that frame, so no frame follows it: a reader knows at once that it
	/// isn't the live one.
	pub ended: bool,
}

impl<'a> Message<'a> for SubscribeStart {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::Subscribe);
	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_track_stream() {
			return Err(DecodeError::Version);
		}
		if !version.has_ranges() {
			return Ok(Self {
				id: 0,
				start: Some(r.varint()?),
				largest: Edge::Unknown,
			});
		}
		let id = r.varint()?;
		let start = r.varint_opt()?;
		let largest = match r.u8()? {
			0 => Edge::Unknown,
			1 => Edge::Empty,
			2 => Edge::Largest(Largest {
				position: crate::track::Position {
					group: r.varint()?,
					frame: r.varint()?,
				},
				ended: match r.u8()? {
					0 => false,
					1 => true,
					_ => return Err(DecodeError::InvalidValue),
				},
			}),
			_ => return Err(DecodeError::InvalidValue),
		};
		Ok(Self { id, start, largest })
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_track_stream() {
			return Err(EncodeError::Version);
		}
		if !version.has_ranges() {
			let (0, Some(group), Edge::Unknown) = (self.id, self.start, self.largest) else {
				return Err(EncodeError::InvalidState);
			};
			return w.varint(group);
		}
		w.varint(self.id)?;
		w.varint_opt(self.start)?;
		match self.largest {
			Edge::Unknown => w.u8(0),
			Edge::Empty => w.u8(1),
			Edge::Largest(largest) => {
				w.u8(2);
				w.varint(largest.position.group)?;
				w.varint(largest.position.frame)?;
				w.u8(largest.ended.into());
			}
		}
		Ok(())
	}
}

/// SUBSCRIBE_END: the exclusive end of a Lite05+ subscription's track.
///
/// No group at or after `group` will be produced. Before lite-07 the publisher FINs the
/// Subscribe Stream after it; lite-07 leaves the stream open for updates, so the
/// subscriber FINs once it wants nothing more. `0` means the track ended
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
/// `R` is how the ranges travel: a [`RangeEdit`] on the wire, and every live range with its
/// Range ID once [`LiveRanges`] resolves the edit, which is what a publisher applies.
///
/// Lite03+ only.
#[derive(Clone, Debug)]
pub struct SubscribeUpdate<R = RangeEdit> {
	/// A join requesting this group or later, judged against the local refresh ratio.
	pub wanted_group: Option<u64>,
	pub priority: i8,
	/// See [`Subscribe::max_delay`].
	pub max_delay: Option<std::time::Duration>,
	/// The change to the ranges; see [`Subscribe::ranges`].
	pub ranges: R,
	/// See [`Subscribe::order`].
	pub order: Order,
}

impl<R> SubscribeUpdate<R> {
	/// The same update with its ranges as `ranges`.
	pub(crate) fn with_ranges<T>(self, ranges: T) -> SubscribeUpdate<T> {
		SubscribeUpdate {
			wanted_group: self.wanted_group,
			priority: self.priority,
			max_delay: self.max_delay,
			ranges,
			order: self.order,
		}
	}
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

		// Lite-07 carries a priority and order per range, which the publisher reads off the
		// live ranges once it applies the edit.
		let (priority, order) = match version.has_ranges() {
			true => (0, Order::Descending),
			false => (super::priority::from_wire(r.u8()?), decode_order(r, version)?),
		};
		let max_delay = decode_max_delay(r, version)?;
		let ranges = match version.has_ranges() {
			true => {
				// Not preallocated, like the ranges: each id costs a byte of the bounded message.
				let mut remove = Vec::new();
				for _ in 0..r.varint()? {
					remove.push(r.varint()?);
				}
				let add = decode_range_list(r)?;
				let mut retune = Vec::new();
				for _ in 0..r.varint()? {
					let id = r.varint()?;
					let policy = decode_policy(r)?;
					let max = r.varint_opt()?.map(std::time::Duration::from_millis);
					retune.push((id, Retune { policy, max }));
				}
				RangeEdit::Edit { remove, add, retune }
			}
			false => RangeEdit::Replace(stamped(decode_ranges(r, version)?, Policy { priority, order })),
		};

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

		if !version.has_ranges() {
			w.u8(super::priority::to_wire(self.priority));
			encode_order(w, version, self.order)?;
		}
		encode_max_delay(w, version, self.max_delay)?;
		match (&self.ranges, version.has_ranges()) {
			(RangeEdit::Edit { remove, add, retune }, true) => {
				w.varint(remove.len() as u64)?;
				for id in remove {
					w.varint(*id)?;
				}
				encode_range_list(w, add)?;
				w.varint(retune.len() as u64)?;
				for (id, retune) in retune {
					w.varint(*id)?;
					encode_policy(w, retune.policy)?;
					w.varint_opt(retune.max.map(crate::track::live_millis))?;
				}
			}
			(RangeEdit::Replace(ranges), false) => encode_ranges(w, version, ranges)?,
			_ => return Err(EncodeError::Version),
		}

		if version.has_refresh() {
			w.varint_opt(self.wanted_group)?;
		}
		Ok(())
	}
}

/// The ranges a SUBSCRIBE_UPDATE carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RangeEdit {
	/// Before lite-07: the whole list, replacing the live one.
	Replace(Vec<Range>),
	/// Lite-07: retire the ranges with these Range IDs, then add these, each taking the next
	/// Range ID, then give each live range named in `retune` that priority and order, and a
	/// live join that max age, in place, keeping where it was placed.
	Edit {
		remove: Vec<u64>,
		add: Vec<Range>,
		retune: Vec<(u64, Retune)>,
	},
}

/// A subscription's live ranges by Range ID, which both ends of a Subscribe Stream keep: the
/// subscriber to write each update as an edit, the publisher to resolve one.
///
/// The SUBSCRIBE's ranges take Range IDs from 0 in order, and each range an update adds
/// takes the next. Before lite-07 an update replaces the list, numbered afresh.
#[derive(Clone, Debug)]
pub(crate) struct LiveRanges {
	version: Version,
	next: u64,
	live: std::collections::BTreeMap<u64, Range>,
	/// The bytes the live ranges take on the wire, held to what one SUBSCRIBE carries.
	size: usize,
}

/// A received update's ranges, resolved against the live ones by Range ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Applied {
	/// Lite-07: the Range IDs retired, then each range added with its Range ID, then each
	/// live join retuned as it now reads, so applying it costs the ranges it names.
	Edit {
		retired: Vec<u64>,
		added: Vec<(u64, Range)>,
		retuned: Vec<(u64, Range)>,
	},
	/// Before lite-07: every live range, numbered afresh.
	Replace(Vec<(u64, Range)>),
}

impl Applied {
	/// This followed by `later`, as one: a range `later` retires that this added never was.
	/// Holds no more than the ranges live before this and after `later`.
	pub fn then(self, later: Self) -> Self {
		match (self, later) {
			(
				Self::Edit {
					mut retired,
					mut added,
					mut retuned,
				},
				Self::Edit {
					retired: gone,
					added: came,
					retuned: tuned,
				},
			) => {
				let gone: std::collections::BTreeSet<u64> = gone.into_iter().collect();
				// Each Range ID is added once, so one this added and `later` retired was never live.
				let fresh: std::collections::BTreeSet<u64> =
					added.iter().map(|(id, _)| *id).filter(|id| gone.contains(id)).collect();
				added.retain(|(id, _)| !fresh.contains(id));
				retuned.retain(|(id, _)| !gone.contains(id));
				retired.extend(gone.difference(&fresh));
				// A later retune of a range this added is that range added as it now reads, and
				// one of a range this retuned replaces that retune.
				for (id, range) in tuned {
					match added.iter_mut().find(|(added, _)| *added == id) {
						Some((_, added)) => *added = range,
						None => {
							retuned.retain(|(tuned, _)| *tuned != id);
							retuned.push((id, range));
						}
					}
				}
				added.extend(came);
				Self::Edit {
					retired,
					added,
					retuned,
				}
			}
			(_, later) => later,
		}
	}
}

impl LiveRanges {
	/// The ranges a SUBSCRIBE carried.
	pub fn new(version: Version, ranges: &[Range]) -> Self {
		let mut live = Self {
			version,
			next: 0,
			live: Default::default(),
			size: 0,
		};
		live.add(ranges.iter().copied());
		live
	}

	fn add(&mut self, ranges: impl IntoIterator<Item = Range>) {
		let form: Form = self.version.into();
		for range in ranges {
			// One past the varint's reach can't come off the wire, and is refused below.
			self.size = self.size.saturating_add(range_size(&range, form).unwrap_or(usize::MAX));
			self.live.insert(self.next, range);
			self.next += 1;
		}
	}

	/// Put `range` back live under the Range ID `id` it already had.
	fn insert(&mut self, id: u64, range: Range) {
		self.size = self
			.size
			.saturating_add(range_size(&range, self.version.into()).unwrap_or(usize::MAX));
		self.live.insert(id, range);
	}

	fn remove(&mut self, id: u64) -> Option<Range> {
		let range = self.live.remove(&id)?;
		self.size = self
			.size
			.saturating_sub(range_size(&range, self.version.into()).unwrap_or(usize::MAX));
		Some(range)
	}

	/// The Range ID the next range added takes.
	pub fn next(&self) -> u64 {
		self.next
	}

	/// Every live range with its Range ID, in Range ID order.
	#[cfg(test)]
	pub fn numbered(&self) -> Vec<(u64, Range)> {
		self.iter().map(|(id, range)| (id, *range)).collect()
	}

	/// Every live range with its Range ID, in Range ID order.
	pub fn iter(&self) -> impl Iterator<Item = (u64, &Range)> {
		self.live.iter().map(|(id, range)| (*id, range))
	}

	/// Whether the publisher reports where each range starts, which lite-07 does by Range
	/// ID. Before it, a start describes the SUBSCRIBE's one range alone.
	pub fn reports_starts(&self) -> bool {
		self.version.has_ranges()
	}

	/// The live range with Range ID `id`.
	pub fn get(&self, id: u64) -> Option<&Range> {
		self.live.get(&id)
	}

	/// Whether `id` was ever assigned.
	pub fn assigned(&self, id: u64) -> bool {
		id < self.next
	}

	/// Resolve a received edit by Range ID, costing the ranges it names.
	pub fn apply(&mut self, edit: RangeEdit) -> Result<Applied, DecodeError> {
		let applied = match edit {
			RangeEdit::Replace(ranges) if !self.version.has_ranges() => {
				*self = Self::new(self.version, &ranges);
				Applied::Replace(self.iter().map(|(id, range)| (id, *range)).collect())
			}
			RangeEdit::Edit { remove, add, retune } if self.version.has_ranges() => {
				for id in &remove {
					// An id never assigned, or already retired (including earlier in this list).
					self.remove(*id).ok_or(DecodeError::InvalidValue)?;
				}
				// Retuned before the adds, so a retune names a range live before this edit.
				let mut retuned: Vec<(u64, Range)> = Vec::with_capacity(retune.len().min(self.live.len()));
				for (id, retune) in retune {
					// Each range once: one retuned twice in an edit says nothing a single one doesn't.
					if retuned.iter().any(|(tuned, _)| *tuned == id) {
						return Err(DecodeError::InvalidValue);
					}
					let mut range = self.remove(id).ok_or(DecodeError::InvalidValue)?;
					match &mut range.start {
						Some(Anchor::Live { max: live, .. } | Anchor::Joined { max: live, .. }) => *live = retune.max,
						// Only a live join has a max age to change.
						_ if retune.max.is_some() => return Err(DecodeError::InvalidValue),
						_ => {}
					}
					range = range.with_policy(retune.policy);
					self.insert(id, range);
					retuned.push((id, range));
				}
				let added = (self.next..).zip(add.iter().copied()).collect();
				self.add(add);
				Applied::Edit {
					retired: remove,
					added,
					retuned,
				}
			}
			_ => return Err(DecodeError::Version),
		};
		// Asking for nothing is refused like a SUBSCRIBE that does, and a set no SUBSCRIBE
		// could carry like one too large to send.
		if self.live.is_empty() {
			return Err(DecodeError::InvalidSubscribeLocation);
		}
		if self.size > super::message::MAX_MESSAGE_SIZE {
			return Err(DecodeError::TooMany);
		}
		Ok(applied)
	}

	/// The edit that keeps the live ranges as they are.
	pub fn unchanged(&self) -> RangeEdit {
		match self.version.has_ranges() {
			true => RangeEdit::Edit {
				remove: Vec::new(),
				add: Vec::new(),
				retune: Vec::new(),
			},
			false => RangeEdit::Replace(self.live.values().copied().collect()),
		}
	}

	/// The edit that turns the live ranges into `ranges`, which become the live ones.
	///
	/// A range already live keeps its Range ID, so an update that changes one range sends
	/// only that one, and the publisher keeps where the rest resolved.
	#[cfg(test)]
	pub fn edit(&mut self, ranges: &[Range]) -> Result<RangeEdit, EncodeError> {
		let keyed: Vec<_> = ranges.iter().map(|range| ((), *range)).collect();
		self.edit_keyed(&keyed, |_| ()).map(|(edit, _)| edit)
	}

	/// [`Self::edit`] for ranges asked for by different owners: a live range keeps its Range
	/// ID only for a wanted range of the same key, `key_of` giving each live one's. A live join
	/// wanted under another max age alone, with the same key, is retuned in place rather than
	/// placed afresh. Returns the edit and the Range ID each range it adds takes, with that
	/// range's key.
	pub fn edit_keyed<K: Copy + Eq + std::hash::Hash>(
		&mut self,
		ranges: &[(K, Range)],
		key_of: impl Fn(u64) -> K,
	) -> Result<(RangeEdit, Vec<(u64, K)>), EncodeError> {
		if ranges.is_empty() {
			return Err(EncodeError::InvalidState);
		}
		if !self.version.has_ranges() {
			// Replaced ranges take fresh Range IDs, so nothing said of the old ones applies.
			let ranges: Vec<Range> = ranges.iter().map(|(_, range)| *range).collect();
			if !self.live.values().eq(&ranges) {
				self.live.clear();
				self.size = 0;
				self.add(ranges.iter().copied());
			}
			return Ok((RangeEdit::Replace(ranges), Vec::new()));
		}
		// Each live range keeps at most one wanted copy, so duplicates edit like any other.
		let mut wanted = std::collections::HashMap::<(K, Range), usize>::new();
		for wanted_range in ranges {
			*wanted.entry(*wanted_range).or_default() += 1;
		}
		let mut remove = Vec::new();
		for (&id, range) in &self.live {
			match wanted.get_mut(&(key_of(id), *range)) {
				Some(count) if *count > 0 => *count -= 1,
				_ => remove.push(id),
			}
		}
		// In the order asked for, so the publisher numbers them the way this side does.
		let mut added: Vec<(K, Range)> = ranges
			.iter()
			.filter(|wanted_range| {
				let count = wanted.get_mut(*wanted_range).expect("counted above");
				let keep = *count > 0;
				*count = count.saturating_sub(1);
				keep
			})
			.copied()
			.collect();
		// A range wanted again under another priority or order, or a live join under another
		// max age, its placement kept.
		let mut retune = Vec::new();
		remove.retain(|&id| {
			let key = key_of(id);
			let live = self.live[&id];
			let found = (added.iter()).position(|(wanted_key, range)| *wanted_key == key && live.retunes(range));
			let Some(index) = found else {
				return true;
			};
			let (_, range) = added.remove(index);
			let max = range.max().flatten();
			retune.push((
				id,
				range,
				Retune {
					policy: range.policy(),
					max,
				},
			));
			false
		});
		let add: Vec<Range> = added.iter().map(|(_, range)| *range).collect();
		// Checked before anything changes, so a refused edit leaves the ranges as last sent.
		let form = self.version.into();
		let size = |range: &Range| range_size(range, form).unwrap_or(usize::MAX);
		let removed = (remove.iter().chain(retune.iter().map(|(id, _, _)| id)))
			.map(|id| size(&self.live[id]))
			.fold(0, usize::saturating_add);
		let size = (add.iter().chain(retune.iter().map(|(_, range, _)| range)))
			.map(size)
			.fold(self.size.saturating_sub(removed), usize::saturating_add);
		if size > super::message::MAX_MESSAGE_SIZE {
			return Err(EncodeError::TooLarge);
		}
		for &id in &remove {
			self.remove(id);
		}
		for &(id, range, _) in &retune {
			self.remove(id);
			self.insert(id, range);
		}
		let ids = (self.next..).zip(added.into_iter().map(|(key, _)| key)).collect();
		self.add(add.iter().copied());
		let retune = retune.into_iter().map(|(id, _, max)| (id, max)).collect();
		Ok((RangeEdit::Edit { remove, add, retune }, ids))
	}
}

/// Names groups that will not be delivered: aborted or reset, evicted or expired with no copy
/// left, stale, or (on lite-07 with [`StreamError::Skip`]) starts the publisher declared it will
/// never produce. Every drop is final on its subscription, a stale (OLD) one included: a
/// subscriber that still wants a group asks for it on a request of its own.
///
/// Lite03+; publishers send it on Lite05 and later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscribeDrop {
	/// The dropped group keys.
	pub groups: std::ops::Range<u64>,

	/// Why the groups were given up: a stream error code on lite-07 and later, or an
	/// application-specific code before, where 0 means simply unavailable.
	pub error: u64,

	/// Lite-07 with `OLD`: the `Max Age` the groups were judged stale under, or `None` for the
	/// publisher's own, which settles every reader. Ignored for any other drop.
	pub max_age: Option<std::time::Duration>,
}

#[cfg(feature = "async")]
impl SubscribeDrop {
	/// Name `groups` given up with `err`, as `version` carries it.
	///
	/// An `OLD` drop named here depends on no reader's `Max Age`, so it settles every reader as
	/// the publisher's own does. [`Self::stale`] names the one a staleness verdict was judged
	/// under.
	pub fn new(groups: std::ops::Range<u64>, err: &StreamError, version: Version) -> Self {
		Self {
			groups,
			// Before lite-07 the code is the application's, so it says nothing of why.
			error: match version.has_drop_code() {
				true => version.encode_stream_code(err).into(),
				false => 0,
			},
			max_age: None,
		}
	}

	/// Name `groups` judged stale under `max_age`, `None` for the publisher's own.
	pub fn stale(groups: std::ops::Range<u64>, max_age: Option<std::time::Duration>, version: Version) -> Self {
		Self {
			max_age,
			..Self::new(groups, &StreamError::Old, version)
		}
	}

	/// Name `groups` given up as `hole`: lite-07 skips what was never produced with `SKIP`,
	/// and earlier versions drop it as not found. A stale verdict names the budget it was
	/// judged under, no limit being the publisher's own max age; an `OLD` drop with no verdict
	/// of its own, such as one shed to make room, names `unjudged`.
	pub(crate) fn given_up(
		groups: std::ops::Range<u64>,
		hole: &crate::track::Hole,
		unjudged: Option<std::time::Duration>,
		version: Version,
	) -> Self {
		use crate::track::Hole;
		match hole {
			Hole::Skip if version.has_skip() => Self::new(groups, &StreamError::Skip, version),
			hole => match hole.stale_budget() {
				Some(budget) => Self::stale(groups, crate::track::max_of(budget), version),
				// The max age goes out only with an OLD code.
				None => Self {
					max_age: unjudged,
					..Self::new(groups, &hole.error(), version)
				},
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

/// A SUBSCRIBE_DROP range: `Group Start` and an exclusive `Group End` past
/// it on lite-07, an inclusive `Group End` before. Either way it ends by the reserved key
/// `u64::MAX`, so it never names that group.
fn decode_groups(r: &mut Decoder<'_>, version: Version) -> Result<std::ops::Range<u64>, DecodeError> {
	let start = r.varint()?;
	let end = match version.has_group_span() {
		true => Some(r.varint()?).filter(|end| *end > start),
		false => r.varint()?.checked_add(1).filter(|end| *end >= start),
	};
	Ok(start..end.ok_or(DecodeError::InvalidValue)?)
}

fn encode_groups(w: &mut Encoder<'_>, groups: &std::ops::Range<u64>, version: Version) -> Result<(), EncodeError> {
	if groups.start > groups.end {
		return Err(EncodeError::InvalidState);
	}
	w.varint(groups.start)?;
	match version.has_group_span() {
		// A span names at least one key.
		true if groups.is_empty() => Err(EncodeError::InvalidState),
		true => w.varint(groups.end),
		// An empty range at 0 has no inclusive end to spell.
		false => w.varint(groups.end.checked_sub(1).ok_or(EncodeError::InvalidState)?),
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

		let groups = decode_groups(r, version)?;
		let error = r.varint()?;
		let max_age = match is_stale_code(version, error) {
			true => r.varint_opt()?.map(std::time::Duration::from_millis),
			false => None,
		};
		Ok(Self { groups, error, max_age })
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {
				return Err(EncodeError::Version);
			}
			_ => {}
		}

		encode_groups(w, &self.groups, version)?;
		w.varint(self.error)?;
		if is_stale_code(version, self.error) {
			w.varint(wire_millis(self.max_age))?;
		}

		Ok(())
	}
}

/// Whether a SUBSCRIBE_DROP with Error Code `error` carries the `Max Age` it was judged
/// under: an `OLD` drop on lite-07 and later.
fn is_stale_code(version: Version, error: u64) -> bool {
	version.has_judged_drop() && error == u64::from(StreamError::Old.to_code())
}

/// A response message on the subscribe stream, prefixed with a type discriminator
/// on Lite03+.
///
/// The discriminator is version-dependent:
/// - Lite03/04: `0x0` SUBSCRIBE_OK, `0x1` SUBSCRIBE_DROP.
/// - Lite05/06: `0x0` SUBSCRIBE_START, `0x1` SUBSCRIBE_END, `0x2` SUBSCRIBE_DROP
///   (SUBSCRIBE_OK was removed; acceptance is implicit).
/// - Lite07+: `0x1` SUBSCRIBE_END, `0x2` SUBSCRIBE_DROP (carrying `SKIP` for declared skips),
///   and `0x4` SUBSCRIBE_START, one per range.
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
			// Lite05+: SUBSCRIBE_OK is gone; START/END/DROP carry the resolved range. Lite-07
			// answers each range with its own START, under a type of its own.
			_ => match self {
				Self::Start(start) if version.has_ranges() => encode_typed(w, 4, start, version)?,
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
					0 if !version.has_ranges() => Ok(Self::Start(SubscribeStart::decode(buf, version)?)),
					1 => Ok(Self::End(SubscribeEnd::decode(buf, version)?)),
					2 => Ok(Self::Drop(SubscribeDrop::decode(buf, version)?)),
					4 if version.has_ranges() => Ok(Self::Start(SubscribeStart::decode(buf, version)?)),
					_ => Err(DecodeError::InvalidMessage(typ)),
				}
			}
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	/// Before lite-07 a range is an inclusive Group Start and Group End, refused reversed.
	#[test]
	fn drop_ranges_before_lite07_are_inclusive() {
		for version in [Version::Lite05, Version::Lite06] {
			// Group Start 3, Group End 1.
			assert!(matches!(
				SubscribeDrop::decode_slice(&[3, 3, 1, 0], version),
				Err(DecodeError::InvalidValue)
			));
			// Group Start 3, Group End 2: empty.
			let (decoded, _) = SubscribeDrop::decode_slice(&[3, 3, 2, 0], version).unwrap();
			assert_eq!(decoded.groups, 3..3);
			let max = (1 << 62) - 1;
			for groups in [2..2, 1..4, max..max + 1] {
				let msg = SubscribeDrop {
					groups: groups.clone(),
					error: 0,
					max_age: None,
				};
				let data = msg.encode_bytes(version).unwrap();
				assert_eq!(SubscribeDrop::decode_slice(&data, version).unwrap().0.groups, groups);
			}
		}
	}

	/// Lite-07 carries Group Start and an exclusive Group End, which reaches but never passes
	/// the reserved group `u64::MAX`. A span names at least one key: an End not past the
	/// Start is malformed, and so is encoding an empty span.
	#[test]
	fn spans_on_lite07_name_at_least_one_key() {
		let skip = u64::from(StreamError::Skip.to_code());
		for groups in [3..4, 0..u64::MAX, u64::MAX - 1..u64::MAX] {
			let msg = SubscribeDrop {
				groups: groups.clone(),
				error: skip,
				max_age: None,
			};
			let data = msg.encode_bytes(Version::Lite07).unwrap();
			let decoded = SubscribeDrop::decode_slice(&data, Version::Lite07).unwrap().0;
			assert_eq!(decoded.groups, groups);
			assert_eq!(decoded.error, skip);
		}
		// Group Start 3, Group End 3 and 2: empty and reversed.
		for end in [3, 2] {
			assert!(matches!(
				SubscribeDrop::decode_slice(&[3, 3, end, 0], Version::Lite07),
				Err(DecodeError::InvalidValue)
			));
		}
		assert!(
			SubscribeDrop {
				groups: 3..3,
				error: skip,
				max_age: None,
			}
			.encode_bytes(Version::Lite07)
			.is_err()
		);
		for version in [Version::Lite06, Version::Lite07] {
			let reversed = SubscribeDrop {
				groups: std::ops::Range { start: 3, end: 1 },
				error: 0,
				max_age: None,
			};
			assert!(reversed.encode_bytes(version).is_err());
		}
	}

	#[test]
	fn subscribe_start_roundtrips_on_lite05() {
		let resp = SubscribeResponse::Start(SubscribeStart {
			id: 0,
			start: Some(42),
			largest: Edge::Unknown,
		});
		let mut buf = Vec::new();
		resp.encode(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05)
			.unwrap();
		let mut slice = buf.as_slice();
		match crate::coding::decode_buf(&mut slice, Version::Lite05, |r, v| SubscribeResponse::decode(r, v)).unwrap() {
			SubscribeResponse::Start(start) => assert_eq!(start.start, Some(42)),
			other => panic!("expected Start, got {other:?}"),
		}
		// The one start answers the one range, with nothing of the largest.
		for start in [
			SubscribeStart {
				id: 1,
				start: Some(42),
				largest: Edge::Unknown,
			},
			SubscribeStart {
				id: 0,
				start: None,
				largest: Edge::Unknown,
			},
			SubscribeStart {
				id: 0,
				start: Some(42),
				largest: Edge::Empty,
			},
		] {
			assert!(SubscribeResponse::Start(start).encode_bytes(Version::Lite06).is_err());
		}
	}

	/// Lite-07 answers one range per SUBSCRIBE_START, type 4, with a typed largest: type 0
	/// is no message there.
	#[test]
	fn subscribe_start_answers_one_range_on_lite07() {
		let position = crate::track::Position { group: 3, frame: 2 };
		for (largest, tail) in [
			(Edge::Unknown, &[0][..]),
			(Edge::Empty, &[1][..]),
			(Edge::Largest(Largest { position, ended: false }), &[2, 3, 2, 0][..]),
			(Edge::Largest(Largest { position, ended: true }), &[2, 3, 2, 1][..]),
		] {
			for start in [None, Some(4)] {
				let msg = SubscribeStart { id: 5, start, largest };
				let buf = SubscribeResponse::Start(msg.clone())
					.encode_bytes(Version::Lite07)
					.unwrap();
				let head = [4, 2 + tail.len() as u8, 5, start.map_or(0, |start| start as u8 + 1)];
				assert_eq!(buf.as_ref(), [&head[..], tail].concat());
				match SubscribeResponse::decode_slice(&buf, Version::Lite07).unwrap().0 {
					SubscribeResponse::Start(decoded) => assert_eq!(decoded, msg),
					other => panic!("expected Start, got {other:?}"),
				}
			}
		}
		// Any other largest type, or ended, is malformed.
		for bad in [[4, 3, 5, 0, 3].to_vec(), [4, 6, 5, 0, 2, 3, 2, 2].to_vec()] {
			assert!(SubscribeResponse::decode_slice(&bad, Version::Lite07).is_err());
		}
		// Type 0, lite-06's start, is unknown on lite-07, and type 4 is unknown before it.
		assert!(SubscribeResponse::decode_slice(&[0, 1, 4], Version::Lite07).is_err());
		assert!(SubscribeResponse::decode_slice(&[4, 3, 5, 0, 0], Version::Lite06).is_err());
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
			groups: 1..4,
			error: 7,
			max_age: None,
		});
		let mut buf = Vec::new();
		resp.encode(&mut Encoder::new(&mut buf, Version::Lite07.into()), Version::Lite07)
			.unwrap();
		// Groups 1 to 3 as a start and an exclusive end.
		assert_eq!(buf, [2, 3, 1, 4, 7]);
		match crate::coding::decode_buf(&mut buf.as_slice(), Version::Lite07, |r, v| {
			SubscribeResponse::decode(r, v)
		})
		.unwrap()
		{
			SubscribeResponse::Drop(drop) => assert_eq!((drop.groups, drop.error), (1..4, 7)),
			other => panic!("expected Drop, got {other:?}"),
		}
	}

	/// A lite-07 OLD drop carries the max age it was judged under, in milliseconds plus one, so
	/// zero names the publisher's own. No other drop, and no earlier version, carries one.
	#[cfg(feature = "async")]
	#[test]
	fn a_lite07_old_drop_carries_its_judged_max_age() {
		let old = u64::from(StreamError::Old.to_code());
		for (max_age, wire) in [
			(None, 0u8),
			(Some(std::time::Duration::ZERO), 1),
			(Some(std::time::Duration::from_millis(9)), 10),
		] {
			let msg = SubscribeDrop::stale(1..4, max_age, Version::Lite07);
			let buf = msg.encode_bytes(Version::Lite07).unwrap();
			assert_eq!(buf.as_ref(), [4, 1, 4, old as u8, wire]);
			let decoded = SubscribeDrop::decode_slice(&buf, Version::Lite07).unwrap().0;
			assert_eq!(decoded, msg);
		}
		// A drop depending on no budget settles every reader.
		assert_eq!(
			SubscribeDrop::new(1..4, &StreamError::Old, Version::Lite07).max_age,
			None
		);
		let skip = SubscribeDrop::new(1..4, &StreamError::Skip, Version::Lite07);
		assert_eq!(skip.encode_bytes(Version::Lite07).unwrap().as_ref(), [3, 1, 4, 0x3a]);
		let lite06 = SubscribeDrop::stale(1..4, Some(std::time::Duration::from_millis(9)), Version::Lite06);
		let buf = lite06.encode_bytes(Version::Lite06).unwrap();
		assert_eq!(buf.as_ref(), [3, 1, 3, 0]);
	}

	/// Lite-07 carries skips in SUBSCRIBE_DROP with error code SKIP (0x3a); type 3 is unknown.
	#[test]
	fn subscribe_drop_skip_and_type_3_unknown() {
		let skip = u64::from(StreamError::Skip.to_code());
		let resp = SubscribeResponse::Drop(SubscribeDrop {
			groups: 1..4,
			error: skip,
			max_age: None,
		});
		let buf = resp.encode_bytes(Version::Lite07).unwrap();
		assert_eq!(buf.as_ref(), [2, 3, 1, 4, 0x3a]);
		match SubscribeResponse::decode_slice(&buf, Version::Lite07).unwrap().0 {
			SubscribeResponse::Drop(drop) => {
				assert_eq!(drop.groups, 1..4);
				assert_eq!(drop.error, skip);
			}
			other => panic!("expected Drop, got {other:?}"),
		}
		for version in [Version::Lite06, Version::Lite07] {
			assert!(matches!(
				SubscribeResponse::decode_slice(&[3, 2, 1, 4], version),
				Err(DecodeError::InvalidMessage(3))
			));
		}
	}

	#[test]
	fn subscribe_drop_is_type_2_on_lite05() {
		let resp = SubscribeResponse::Drop(SubscribeDrop {
			groups: 1..4,
			error: 0,
			max_age: None,
		});
		let mut buf = Vec::new();
		resp.encode(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05)
			.unwrap();
		// Type discriminator is the first varint; on Lite05 DROP is 0x2.
		assert_eq!(buf[0], 2);

		let mut slice = buf.as_slice();
		match crate::coding::decode_buf(&mut slice, Version::Lite05, |r, v| SubscribeResponse::decode(r, v)).unwrap() {
			SubscribeResponse::Drop(drop) => assert_eq!(drop.groups, 1..4),
			other => panic!("expected Drop, got {other:?}"),
		}
	}

	#[test]
	fn subscribe_drop_is_type_1_on_lite04() {
		let resp = SubscribeResponse::Drop(SubscribeDrop {
			groups: 1..4,
			error: 0,
			max_age: None,
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
			identity: None,
			node: None,
			id: 1,
			broadcast: Path::from_static("room"),
			track: Cow::Borrowed("video"),
			priority: 3,
			max_delay: Some(std::time::Duration::from_millis(250)),
			// Groups 7 through 9.
			ranges: vec![Range {
				start: absolute(7, 0),
				end: absolute(10, 0),
				..Range::OPEN
			}],
		}
	}

	/// The sample bounded partway through its groups, as only lite-06 carries.
	fn framed_sample() -> Subscribe<'static> {
		Subscribe {
			// Group 7 from frame 4, through frame 2 of group 9.
			ranges: vec![Range {
				start: absolute(7, 4),
				end: absolute(9, 3),
				..Range::OPEN
			}],
			..subscribe_sample()
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

	/// `ranges` as a wire before lite-07 decodes them: at `msg`'s one priority and order.
	fn legacy(msg: &Subscribe<'_>, ranges: impl IntoIterator<Item = Range>) -> Vec<Range> {
		let policy = Policy {
			priority: msg.priority,
			order: msg.order,
		};
		ranges.into_iter().map(|range| range.with_policy(policy)).collect()
	}

	fn encode(msg: &Subscribe<'_>, version: Version) -> Result<Vec<u8>, EncodeError> {
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)?;
		Ok(buf)
	}

	/// Only lite-07 names the origin node; older versions decode as letting the
	/// publisher select.
	#[test]
	fn node_roundtrips_only_on_lite07() {
		let mut msg = subscribe_sample();
		msg.node = Some(crate::route::NodeId {
			relay: 0x7a3f,
			marked: false,
		});
		assert_eq!(
			roundtrip(&msg, Version::Lite07).node,
			Some(crate::route::NodeId {
				relay: 0x7a3f,
				marked: false,
			})
		);
		assert_eq!(roundtrip(&msg, Version::Lite06).node, None);
	}

	/// Lite-06 carries one range's frame bounds; an inclusive `Frame End` on the wire is the
	/// exclusive position just past it here.
	#[test]
	fn subscribe_frame_bounds_roundtrip() {
		let msg = framed_sample();
		assert_eq!(
			roundtrip(&msg, Version::Lite06).ranges,
			legacy(&msg, msg.ranges.clone())
		);
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
			..Range::OPEN
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
		assert_eq!(got.ranges, legacy(&msg, msg.ranges.clone()));
	}

	/// Lite06 carries the raw floor; pre-06 wires encode the key + 1 with 0 meaning
	/// the latest group. An explicit group 0 is that key, so it round-trips as group 0
	/// rather than as the latest group.
	#[test]
	fn group_start_is_absolute_on_lite06() {
		let mut msg = subscribe_sample();
		msg.ranges = vec![Range {
			start: absolute(7, 0),
			end: None,
			..Range::OPEN
		}];

		let lite05 = encode(&msg, Version::Lite05).unwrap();
		let lite06 = encode(&msg, Version::Lite06).unwrap();
		assert_eq!(
			roundtrip(&msg, Version::Lite05).ranges,
			legacy(&msg, msg.ranges.clone())
		);
		assert_eq!(
			roundtrip(&msg, Version::Lite06).ranges,
			legacy(&msg, msg.ranges.clone())
		);
		// The raw byte differs: 7 on the wire, not 7 + 1.
		assert_ne!(lite05, lite06);

		// No floor and a floor of 0 are the same absence of a constraint on lite-06:
		// byte-identical on the wire, and decoded as no start.
		msg.ranges = vec![Range::OPEN];
		let absent = encode(&msg, Version::Lite06).unwrap();
		msg.ranges[0].start = absolute(0, 0);
		let zero = encode(&msg, Version::Lite06).unwrap();
		assert_eq!(absent, zero);
		let got = crate::coding::decode_from(&mut zero.as_slice(), Version::Lite06, |r, v| {
			Subscribe::decode_msg(r, v)
		})
		.unwrap();
		assert_eq!(got.ranges, legacy(&msg, [Range::OPEN]));

		// On the pre-06 wire an explicit group 0 is key + 1, distinct from an
		// absent start (the latest group).
		let explicit = encode(&msg, Version::Lite05).unwrap();
		assert_eq!(
			roundtrip(&msg, Version::Lite05).ranges,
			legacy(&msg, msg.ranges.clone())
		);
		msg.ranges = vec![Range::OPEN];
		let latest = encode(&msg, Version::Lite05).unwrap();
		assert_ne!(explicit, latest);
		assert_eq!(roundtrip(&msg, Version::Lite05).ranges, legacy(&msg, [Range::OPEN]));
		assert_eq!(encode(&msg, Version::Lite06).unwrap(), absent);
	}

	/// A subscription can resume partway through group 0 (a catalog never leaves it), so
	/// a `Frame Start` qualifying the zero floor must survive the round trip.
	#[test]
	fn frame_start_may_qualify_group_zero_on_lite06() {
		let mut msg = framed_sample();
		msg.ranges[0].start = absolute(0, 4);
		assert_eq!(
			roundtrip(&msg, Version::Lite06).ranges,
			legacy(&msg, msg.ranges.clone())
		);
	}

	/// Silently widening to the whole group would deliver frames the caller excluded.
	#[test]
	fn subscribe_frame_bounds_rejected_before_lite06() {
		assert!(encode(&framed_sample(), Version::Lite05).is_err());
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

	/// Lite-07 carries every range, each end at a time or open.
	#[test]
	fn ranges_roundtrip_on_lite07() {
		let mut msg = subscribe_sample();
		msg.ranges = vec![
			Range::OPEN,
			Range {
				start: None,
				end: Some(Anchor::Time(2)),
				..Range::OPEN
			},
			Range {
				start: Some(Anchor::Time(1_500)),
				end: Some(Anchor::Time(3_500)),
				..Range::OPEN
			},
			Range {
				start: Some(Anchor::Time(0)),
				end: Some(Anchor::Time(u64::MAX)),
				..Range::OPEN
			},
		];
		assert_eq!(roundtrip(&msg, Version::Lite07).ranges, msg.ranges);

		// An update from the oldest group to the same ranges adds every one of them.
		let mut sent = LiveRanges::new(Version::Lite07, &[Range::OPEN]);
		let update = SubscribeUpdate {
			wanted_group: None,
			priority: 3,
			max_delay: Some(std::time::Duration::from_millis(250)),
			ranges: sent.edit(&msg.ranges).unwrap(),
			order: crate::track::Order::Ascending,
		};
		let buf = update.encode_bytes(Version::Lite07).unwrap();
		let got = crate::coding::decode_buf(&mut buf.as_ref(), Version::Lite07, |r, v| SubscribeUpdate::decode(r, v))
			.unwrap();
		let mut live = LiveRanges::new(Version::Lite07, &[Range::OPEN]);
		live.apply(got.ranges).unwrap();
		let resolved: Vec<_> = live.iter().map(|(_, range)| *range).collect();
		assert_eq!(resolved, msg.ranges);
	}

	/// Lite-07 carries `Anchor::Absolute` as Position (type 2), allowing frame 0 and exact
	/// group 0 without underflow (separate from `Anchor::Time`, type 1).
	#[test]
	fn lite07_roundtrips_position_at_frame_zero() {
		let mut msg = subscribe_sample();
		msg.ranges = vec![Range {
			start: absolute(7, 0),
			end: absolute(10, 0),
			..Range::OPEN
		}];
		assert_eq!(roundtrip(&msg, Version::Lite07).ranges, msg.ranges);
		let wire = encode(&msg, Version::Lite07).unwrap();
		// The count, priority 0 and descending, then type 2, group 7, frame 0, then type 2,
		// group 10, frame 0.
		assert!(wire.ends_with(&[1, 0x80, 0, 2, 7, 0, 2, 10, 0]), "{wire:?}");

		// Group 0 works cleanly without underflow: [Position(0, 0), Position(1, 0)) and
		// even an empty [None, Position(0, 0)).
		msg.ranges = vec![Range {
			start: absolute(0, 0),
			end: absolute(1, 0),
			..Range::OPEN
		}];
		assert_eq!(roundtrip(&msg, Version::Lite07).ranges, msg.ranges);
		msg.ranges = vec![Range {
			start: None,
			end: absolute(0, 0),
			..Range::OPEN
		}];
		assert_eq!(roundtrip(&msg, Version::Lite07).ranges, msg.ranges);
	}

	/// A lite-07 start or end may be a Position (type 2), at frame 0 or partway through a group.
	#[test]
	fn lite07_starts_and_ends_partway_through_a_group() {
		let mut msg = framed_sample();
		assert_eq!(roundtrip(&msg, Version::Lite07).ranges, msg.ranges);
		let framed = encode(&msg, Version::Lite07).unwrap();
		// The policy, then type 2, start 7, frame 4, then type 2, start 9, frame 3, which it
		// stops before.
		assert!(framed.ends_with(&[1, 0x80, 0, 2, 7, 4, 2, 9, 3]), "{framed:?}");
		assert_eq!(range_size(&msg.ranges[0], Version::Lite07.into()), Some(8));
		msg.ranges = vec![Range {
			start: absolute(7, 4),
			end: None,
			..Range::OPEN
		}];
		assert_eq!(roundtrip(&msg, Version::Lite07).ranges, msg.ranges);
		let position = encode(&msg, Version::Lite07).unwrap();
		// The policy, then type 2, start 7, frame 4, then no end.
		assert!(position.ends_with(&[1, 0x80, 0, 2, 7, 4, 0]), "{position:?}");
		assert_eq!(
			range_size(&msg.ranges[0], Version::Lite07.into()),
			Some(6),
			"the size a range takes counts both fields"
		);

		let decode = |tail: &[u8]| {
			let mut wire = position.clone();
			wire.truncate(position.len() - 7);
			wire.extend_from_slice(&[1, 0x80, 0]);
			wire.extend_from_slice(tail);
			crate::coding::decode_from(&mut wire.as_slice(), Version::Lite07, Subscribe::decode_msg).map(|_| ())
		};
		assert!(decode(&[2, 7, 4, 0]).is_ok());
		assert!(decode(&[2, 7, 0, 0]).is_ok());
		assert!(decode(&[1, 7, 2, 9, 4]).is_ok());
		assert!(decode(&[1, 7, 2, 9, 0]).is_ok());
	}

	/// An update sends only the ranges that changed, and the publisher resolves the same list,
	/// with the same Range IDs, as the subscriber holds.
	#[test]
	fn lite07_updates_edit_only_the_changed_ranges() {
		let version = Version::Lite07;
		let (a, b, c) = (
			Range {
				start: Some(Anchor::Time(1)),
				end: Some(Anchor::Time(4)),
				..Range::OPEN
			},
			Range {
				start: Some(Anchor::Time(3)),
				end: None,
				..Range::OPEN
			},
			Range {
				start: Some(Anchor::Time(9_000)),
				end: None,
				..Range::OPEN
			},
		);
		let mut sent = LiveRanges::new(version, &[a, b]);
		let mut live = LiveRanges::new(version, &[a, b]);
		let mut step = |ranges: &[Range], expected: RangeEdit| {
			let edit = sent.edit(ranges).unwrap();
			assert_eq!(edit, expected);
			let update = SubscribeUpdate {
				wanted_group: None,
				priority: 0,
				max_delay: Some(std::time::Duration::ZERO),
				ranges: edit,
				order: crate::track::Order::Descending,
			};
			let buf = update.encode_bytes(version).unwrap();
			let got =
				crate::coding::decode_buf(&mut buf.as_ref(), version, |r, v| SubscribeUpdate::decode(r, v)).unwrap();
			live.apply(got.ranges).unwrap();
			assert_eq!(live.numbered(), sent.numbered());
		};

		let edit = |remove: &[u64], add: &[Range]| RangeEdit::Edit {
			remove: remove.to_vec(),
			add: add.to_vec(),
			retune: Vec::new(),
		};
		step(&[b, a], edit(&[], &[]));
		step(&[a, c], edit(&[1], &[c]));
		step(&[c, c, a], edit(&[], &[c]));
		step(&[b], edit(&[0, 2, 3], &[b]));
		assert_eq!(live.numbered(), [(4, b)]);
	}

	/// An edit naming a Range ID never assigned or already retired, one leaving nothing
	/// live, or one adding more than a SUBSCRIBE carries, is refused.
	#[test]
	fn lite07_refuses_edits_it_cannot_apply() {
		let version = Version::Lite07;
		let edit = |remove: Vec<u64>, add: Vec<Range>| RangeEdit::Edit {
			remove,
			add,
			retune: Vec::new(),
		};
		let fresh = || LiveRanges::new(version, &[Range::OPEN]);
		assert!(
			fresh().apply(edit(vec![1], vec![Range::OPEN])).is_err(),
			"never assigned"
		);
		assert!(
			fresh().apply(edit(vec![0, 0], vec![Range::OPEN])).is_err(),
			"retired twice"
		);
		assert!(fresh().apply(edit(vec![0], vec![])).is_err(), "nothing left");
		assert!(
			fresh().apply(RangeEdit::Replace(vec![Range::OPEN])).is_err(),
			"not on lite-07"
		);

		let mut live = fresh();
		let one = Range {
			start: absolute(1 << 40, 1 << 40),
			end: absolute(1 << 41, 1 << 40),
			..Range::OPEN
		};
		// Each update fits in a message, but together they would not fit in a SUBSCRIBE.
		let mut refused = false;
		for _ in 0..(super::super::message::MAX_MESSAGE_SIZE / 16) {
			if live.apply(edit(vec![], vec![one; 8])).is_err() {
				refused = true;
				break;
			}
		}
		assert!(refused, "the live ranges are held to one SUBSCRIBE");
	}

	/// An edit resolves to the Range IDs it names, and edits waiting to apply fold into one
	/// that names what stayed retired or added.
	#[test]
	fn lite07_an_edit_names_its_range_ids() {
		let mut live = LiveRanges::new(Version::Lite07, &[Range::OPEN]);
		let range = |group| Range {
			start: absolute(group, 0),
			end: None,
			..Range::OPEN
		};
		let first = live
			.apply(RangeEdit::Edit {
				remove: vec![0],
				add: vec![range(4), range(5)],
				retune: Vec::new(),
			})
			.unwrap();
		assert_eq!(
			first,
			Applied::Edit {
				retired: vec![0],
				added: vec![(1, range(4)), (2, range(5))],
				retuned: Vec::new(),
			}
		);
		let second = live
			.apply(RangeEdit::Edit {
				remove: vec![1],
				add: vec![range(6)],
				retune: Vec::new(),
			})
			.unwrap();
		assert_eq!(
			first.then(second),
			Applied::Edit {
				retired: vec![0],
				added: vec![(2, range(5)), (3, range(6))],
				retuned: Vec::new(),
			}
		);
	}

	/// An edit to ranges too large for a SUBSCRIBE is refused before anything changes, so the
	/// live ranges stay those last sent and the next edit is made against them.
	#[test]
	fn lite07_a_refused_edit_leaves_the_ranges_as_sent() {
		let mut live = LiveRanges::new(Version::Lite07, &[Range::OPEN]);
		let wide: Vec<Range> = (0..super::super::message::MAX_MESSAGE_SIZE as u64 / 4)
			.map(|group| Range {
				start: absolute(group << 20, 0),
				end: absolute((group << 20) + 1, 0),
				..Range::OPEN
			})
			.collect();
		assert!(matches!(live.edit(&wide), Err(EncodeError::TooLarge)));
		assert_eq!(live.numbered(), [(0, Range::OPEN)]);
		assert_eq!(live.next(), 1);
		assert_eq!(
			live.edit(&wide[..2]).unwrap(),
			RangeEdit::Edit {
				remove: vec![0],
				add: wide[..2].to_vec(),
				retune: Vec::new(),
			}
		);
	}

	/// A subscription asking for nothing is refused both ways, and an unknown anchor type
	/// is malformed.
	#[test]
	fn lite07_refuses_an_empty_list_and_unknown_anchors() {
		let version = Version::Lite07;
		let mut msg = subscribe_sample();
		msg.ranges.clear();
		assert!(matches!(encode(&msg, version), Err(EncodeError::InvalidState)));

		msg.ranges = vec![Range::OPEN];
		let mut buf = encode(&msg, version).unwrap();
		// The list is last: a count of one, the policy, then two anchors of type 0.
		let at = buf.len() - 5;
		assert_eq!(&buf[at..], [1, 0x80, 0, 0, 0]);
		buf[at] = 0;
		buf.truncate(at + 1);
		assert!(matches!(
			crate::coding::decode_from(&mut buf.as_slice(), version, Subscribe::decode_msg),
			Err(DecodeError::InvalidSubscribeLocation)
		));

		// Type 2 was a relative anchor before lite-07 dropped it, and is a Position now, a start
		// or an end; type 3 is Live and type 4 Joined, each a start only; type 5 is assigned to
		// nothing, as a start or an end. Each closes the session with a PROTOCOL_VIOLATION.
		let starts = [5].map(|kind| (2, kind));
		let ends = [3, 4, 5].map(|kind| (1, kind));
		for (back, kind) in starts.into_iter().chain(ends) {
			let mut buf = encode(&msg, version).unwrap();
			let at = buf.len() - back;
			buf[at] = kind;
			let err = crate::coding::decode_from(&mut buf.as_slice(), version, Subscribe::decode_msg).unwrap_err();
			assert!(matches!(err, DecodeError::InvalidValue), "{back} {kind}");
			let session = crate::SessionError::from(&crate::Error::Decode(err));
			assert!(matches!(session, crate::SessionError::ProtocolViolation));
		}
	}

	/// An older wire carries one span with absolute ends, so anything else is refused for
	/// the caller to narrow first.
	#[test]
	fn older_wires_refuse_what_one_span_cannot_carry() {
		let refused = [
			vec![Range::OPEN, Range::OPEN],
			vec![Range {
				start: Some(Anchor::Time(1_500)),
				end: None,
				..Range::OPEN
			}],
			vec![Range {
				start: None,
				end: Some(Anchor::Time(1)),
				..Range::OPEN
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
					max_delay: Some(std::time::Duration::ZERO),
					ranges: LiveRanges::new(version, &[Range::OPEN]).unchanged(),
					order: crate::track::Order::Descending,
				};
				let buf = update.encode_bytes(version).unwrap();
				let got = crate::coding::decode_buf(&mut buf.as_ref(), version, |r, v| SubscribeUpdate::decode(r, v))
					.unwrap();
				assert_eq!(got.wanted_group, if version.has_refresh() { wanted } else { None });
			}
		}
	}

	/// `Subscriber Max Age` is bare milliseconds before lite-07, so no limit goes out as the
	/// largest every peer reads and reads back as no limit. Lite01/02 carry none, and lite-07
	/// none either, since each live start carries its own.
	#[test]
	fn max_delay_is_carried_only_before_lite07() {
		let ms = std::time::Duration::from_millis;
		let encoded = |version: Version, max_delay| {
			let mut buf = Vec::new();
			encode_max_delay(&mut Encoder::new(&mut buf, version.into()), version, max_delay).unwrap();
			let wire = (!buf.is_empty()).then(|| Decoder::new(&buf, version.into()).varint().unwrap());
			let got = decode_max_delay(&mut Decoder::new(&buf, version.into()), version).unwrap();
			(wire, got)
		};
		assert_eq!(encoded(Version::Lite07, Some(ms(250))), (None, None));
		assert_eq!(encoded(Version::Lite06, Some(ms(250))), (Some(250), Some(ms(250))));
		assert_eq!(encoded(Version::Lite06, Some(ms(0))), (Some(0), Some(ms(0))));
		assert_eq!(encoded(Version::Lite06, None), (Some(MAX_SAFE_DELAY_MS), None));
		assert_eq!(
			encoded(Version::Lite06, Some(ms(MAX_SAFE_DELAY_MS - 1))),
			(Some(MAX_SAFE_DELAY_MS - 1), Some(ms(MAX_SAFE_DELAY_MS - 1)))
		);

		let mut msg = subscribe_sample();
		msg.max_delay = Some(ms(250));
		for version in [Version::Lite01, Version::Lite02, Version::Lite07] {
			assert_eq!(roundtrip(&msg, version).max_delay, None, "{version}");
		}
		assert_eq!(roundtrip(&msg, Version::Lite03).max_delay, Some(ms(250)));
	}

	/// A live start's max age rides beside its lag, in milliseconds plus one, rounded up, 0 for
	/// no limit.
	#[test]
	fn a_live_starts_max_age_roundtrips_on_lite07() {
		let ms = std::time::Duration::from_millis;
		let live = |max| Range {
			start: Some(Anchor::Live {
				min: Some(ms(100)),
				max,
			}),
			end: None,
			..Range::OPEN
		};
		for (max, wire) in [
			(None, None),
			(Some(ms(0)), Some(ms(0))),
			(Some(ms(250)), Some(ms(250))),
			(Some(std::time::Duration::from_micros(1500)), Some(ms(2))),
		] {
			let mut msg = subscribe_sample();
			msg.ranges = vec![live(max)];
			assert_eq!(roundtrip(&msg, Version::Lite07).ranges, vec![live(wire)], "{max:?}");
		}
	}

	/// A placed live join rides as type 4: its group, a frame that may be 0, then its max age
	/// as a live start carries it. Only a start, and only lite-07: an older wire refuses it for
	/// the caller to carry as a position first.
	#[test]
	fn a_joined_start_roundtrips_on_lite07() {
		let ms = std::time::Duration::from_millis;
		let joined = |group, frame, max| Range {
			start: Some(Anchor::Joined {
				at: Position { group, frame },
				max,
			}),
			end: Some(Anchor::Time(9_000)),
			..Range::OPEN
		};
		for (range, wire) in [
			(joined(1_500, 0, None), joined(1_500, 0, None)),
			(joined(1_500, 3, Some(ms(0))), joined(1_500, 3, Some(ms(0)))),
			(joined(0, 7, Some(ms(250))), joined(0, 7, Some(ms(250)))),
			(
				joined(9, 1, Some(std::time::Duration::from_micros(1500))),
				joined(9, 1, Some(ms(2))),
			),
		] {
			let mut msg = subscribe_sample();
			msg.ranges = vec![range];
			assert_eq!(roundtrip(&msg, Version::Lite07).ranges, vec![wire], "{range:?}");
		}

		// The count, the policy, the type, group, frame, and max age plus one, then the end's
		// type and time, 251 and 9000 each in two bytes.
		let mut msg = subscribe_sample();
		msg.ranges = vec![joined(5, 0, Some(ms(250)))];
		let buf = encode(&msg, Version::Lite07).unwrap();
		let tail = [1, 0x80, 0, 4, 5, 0, 0x80, 251, 1, 0xa3, 0x28];
		assert_eq!(&buf[buf.len() - tail.len()..], tail);

		// An end can't be a join, placed or not.
		msg.ranges = vec![Range {
			start: None,
			end: joined(5, 0, None).start,
			..Range::OPEN
		}];
		assert!(matches!(encode(&msg, Version::Lite07), Err(EncodeError::InvalidState)));

		msg.ranges = vec![Range {
			start: joined(5, 1, None).start,
			end: None,
			..Range::OPEN
		}];
		for version in [Version::Lite03, Version::Lite05, Version::Lite06] {
			assert!(matches!(encode(&msg, version), Err(EncodeError::Version)), "{version}");
		}
	}

	/// A placed join retunes its max age in place like a live one, kept where it was placed;
	/// one placed elsewhere is another range.
	#[test]
	fn lite07_retunes_a_placed_join_in_place() {
		let ms = std::time::Duration::from_millis;
		let version = Version::Lite07;
		let joined = |group, max| Range {
			start: Some(Anchor::Joined {
				at: Position { group, frame: 2 },
				max,
			}),
			end: None,
			..Range::OPEN
		};
		let mut sent = LiveRanges::new(version, &[joined(5, Some(ms(500)))]);
		let mut live_side = LiveRanges::new(version, &[joined(5, Some(ms(500)))]);
		let edit = sent.edit(&[joined(5, None)]).unwrap();
		assert_eq!(
			edit,
			RangeEdit::Edit {
				remove: vec![],
				add: vec![],
				retune: vec![(0, Retune::max(None))],
			}
		);
		live_side.apply(edit).unwrap();
		assert_eq!(live_side.numbered(), vec![(0, joined(5, None))]);

		let edit = sent.edit(&[joined(6, None)]).unwrap();
		assert_eq!(
			edit,
			RangeEdit::Edit {
				remove: vec![0],
				add: vec![joined(6, None)],
				retune: vec![],
			}
		);
	}

	/// An update retunes a live join wanted under another max age alone in place, keeping its
	/// Range ID and so where it was placed; anything else about it changing replaces it. The
	/// publisher refuses a retune naming no live join, or one twice.
	#[test]
	fn lite07_retunes_a_live_join_in_place() {
		let ms = std::time::Duration::from_millis;
		let version = Version::Lite07;
		let live = |min, max| Range {
			start: Some(Anchor::Live {
				min: Some(ms(min)),
				max,
			}),
			end: None,
			..Range::OPEN
		};
		let time = Range {
			start: Some(Anchor::Time(5)),
			end: None,
			..Range::OPEN
		};
		let mut sent = LiveRanges::new(version, &[live(100, Some(ms(500))), time]);
		let mut live_side = LiveRanges::new(version, &[live(100, Some(ms(500))), time]);

		let edit = sent.edit(&[live(100, Some(ms(2000))), time]).unwrap();
		assert_eq!(
			edit,
			RangeEdit::Edit {
				remove: vec![],
				add: vec![],
				retune: vec![(0, Retune::max(Some(ms(2000))))],
			}
		);
		let update = SubscribeUpdate {
			wanted_group: None,
			priority: 0,
			max_delay: None,
			ranges: edit,
			order: crate::track::Order::Descending,
		};
		let buf = update.encode_bytes(version).unwrap();
		let got = crate::coding::decode_buf(&mut buf.as_ref(), version, |r, v| SubscribeUpdate::decode(r, v)).unwrap();
		assert_eq!(
			live_side.apply(got.ranges).unwrap(),
			Applied::Edit {
				retired: vec![],
				added: vec![],
				retuned: vec![(0, live(100, Some(ms(2000))))],
			}
		);
		assert_eq!(live_side.numbered(), sent.numbered());

		// Another lag is another join, placed afresh.
		let edit = sent.edit(&[live(200, Some(ms(2000))), time]).unwrap();
		assert_eq!(
			edit,
			RangeEdit::Edit {
				remove: vec![0],
				add: vec![live(200, Some(ms(2000)))],
				retune: vec![],
			}
		);
		live_side.apply(edit).unwrap();

		let retune = |retune| RangeEdit::Edit {
			remove: vec![],
			add: vec![],
			retune,
		};
		let max = |max| Retune::max(max);
		assert!(
			live_side.clone().apply(retune(vec![(1, max(Some(ms(5))))])).is_err(),
			"a time start has no max age"
		);
		assert!(
			live_side.clone().apply(retune(vec![(1, max(None))])).is_ok(),
			"but takes a new policy"
		);
		assert!(
			live_side.clone().apply(retune(vec![(0, max(None))])).is_err(),
			"retired"
		);
		assert!(
			live_side
				.clone()
				.apply(retune(vec![(2, max(None)), (2, max(None))]))
				.is_err(),
			"twice"
		);
		assert!(live_side.apply(retune(vec![(2, max(None))])).is_ok());
	}

	/// Lite-07 carries a live join's lag as a start Anchor in whole milliseconds, rounded up,
	/// or none for no lag limit, on SUBSCRIBE and SUBSCRIBE_UPDATE alike; an end refuses it.
	/// An older wire carries it as no start.
	#[test]
	fn a_live_start_roundtrips_only_on_lite07() {
		let ms = |millis| Some(std::time::Duration::from_millis(millis));
		let live = |min| Range {
			start: Some(Anchor::Live { min, max: None }),
			end: Some(Anchor::Time(9)),
			..Range::OPEN
		};
		for (min, wire) in [
			(ms(0), ms(0)),
			(ms(250), ms(250)),
			(Some(std::time::Duration::from_micros(1500)), ms(2)),
			(None, None),
		] {
			let mut msg = subscribe_sample();
			msg.ranges = vec![live(min)];
			assert_eq!(roundtrip(&msg, Version::Lite07).ranges, vec![live(wire)]);

			let update = SubscribeUpdate {
				wanted_group: None,
				priority: 0,
				max_delay: Some(std::time::Duration::ZERO),
				ranges: LiveRanges::new(Version::Lite07, &[]).edit(&[live(min)]).unwrap(),
				order: crate::track::Order::Descending,
			};
			let buf = update.encode_bytes(Version::Lite07).unwrap();
			let got =
				crate::coding::decode_buf(&mut buf.as_ref(), Version::Lite07, |r, v| SubscribeUpdate::decode(r, v))
					.unwrap();
			assert!(matches!(got.ranges, RangeEdit::Edit { add, .. } if add == [live(wire)]));
		}

		// The lag rides plus one, so zero says no lag limit, as the max age beside it does.
		let anchor = |min| wire_anchor(Some(Anchor::Live { min, max: None }), false).unwrap();
		assert_eq!(anchor(ms(250)), (3, [Some(251), Some(0), None]));
		assert_eq!(anchor(None), (3, [Some(0), Some(0), None]));

		// No start on a wire without the Anchor.
		let mut msg = subscribe_sample();
		msg.ranges = vec![Range {
			start: Some(Anchor::Live {
				min: ms(250),
				max: None,
			}),
			end: None,
			..Range::OPEN
		}];
		assert_eq!(roundtrip(&msg, Version::Lite06).ranges, legacy(&msg, [Range::OPEN]));

		// A live end is refused both ways.
		msg.ranges = vec![Range {
			start: None,
			end: Some(Anchor::Live {
				min: ms(250),
				max: None,
			}),
			..Range::OPEN
		}];
		assert!(msg.encode_bytes(Version::Lite07).is_err());
		let mut buf = Vec::new();
		let mut w = Encoder::new(&mut buf, Version::Lite07.into());
		w.varint(1).unwrap();
		w.u8(0x80);
		w.u8(0);
		for value in [0, 3, 250] {
			w.varint(value).unwrap();
		}
		assert!(matches!(
			decode_range_list(&mut Decoder::new(&buf, Version::Lite07.into())),
			Err(DecodeError::InvalidValue)
		));
	}

	/// Lite-07 carries each range's `Priority` then `Group Order`, on a SUBSCRIBE's ranges and
	/// a SUBSCRIBE_UPDATE's retunes alike, and refuses an order it does not define.
	#[test]
	fn range_policy_roundtrips_on_lite07() {
		use crate::track::Order;

		let version = Version::Lite07;
		for order in [Order::Ascending, Order::Descending] {
			let policy = Policy { priority: -3, order };
			let mut msg = subscribe_sample();
			msg.ranges = vec![Range::OPEN.with_policy(policy)];
			let got = roundtrip(&msg, version);
			assert_eq!(got.ranges[0].policy(), policy);
			// The subscription as a whole runs at its one range's policy.
			assert_eq!((got.priority, got.order), (policy.priority, policy.order));

			let update = SubscribeUpdate {
				wanted_group: None,
				priority: 0,
				max_delay: None,
				ranges: RangeEdit::Edit {
					remove: Vec::new(),
					add: Vec::new(),
					retune: vec![(0, Retune { policy, max: None })],
				},
				order: Order::Descending,
			};
			let mut buf = Vec::new();
			update
				.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)
				.unwrap();
			// The retune ends the edit: its Range ID, priority, order, then no max age, then no
			// wanted group.
			let at = buf.len() - 5;
			assert_eq!(
				&buf[at..],
				[
					0,
					super::super::priority::to_wire(-3),
					u8::from(order == Order::Ascending),
					0,
					0
				]
			);
			let got = crate::coding::decode_buf(&mut buf.as_slice(), version, |r, v| SubscribeUpdate::decode_msg(r, v))
				.unwrap();
			assert_eq!(got.ranges, update.ranges);

			buf[at + 2] = 2;
			assert!(matches!(
				crate::coding::decode_buf(&mut buf.as_slice(), version, |r, v| SubscribeUpdate::decode_msg(r, v)),
				Err(DecodeError::InvalidValue)
			));
		}

		// Ranges at different policies: the subscription runs at the highest priority, newest
		// first if any range there asks so.
		let mut msg = subscribe_sample();
		msg.ranges = vec![
			Range::OPEN.with_policy(Policy {
				priority: 1,
				order: Order::Ascending,
			}),
			Range {
				start: absolute(7, 0),
				end: absolute(10, 0),
				..Range::OPEN
			}
			.with_policy(Policy {
				priority: 5,
				order: Order::Ascending,
			}),
		];
		let got = roundtrip(&msg, version);
		assert_eq!((got.priority, got.order), (5, Order::Ascending));
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
				..Range::OPEN
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
