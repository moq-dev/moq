use crate::coding::*;

use super::{Message, Version};

#[derive(Clone, Debug)]
pub struct Group {
	// The subscribe ID.
	pub subscribe: u64,

	// The group key
	pub key: u64,

	// The index of the first frame on this stream. 0 (the common case) means the stream
	// carries the group from its beginning; a higher value means the leading frames are
	// missing, because the subscription started partway in. Lite06+ only; older versions
	// always start at 0.
	pub frame_start: u64,

	// How many keys just below `key` the publisher proves hold no group, as lite-07's
	// `Previous Delta` says: every key below it on the track's first group. Earlier versions
	// carry none.
	pub gap: u64,
}

impl Group {
	/// The varints the newest layout carries: Subscribe ID, Group Start, Frame Start and
	/// Previous Delta.
	const FIELDS: usize = 4;

	/// The keys just below this group that hold no group.
	pub fn empty_below(&self) -> std::ops::Range<u64> {
		self.key - self.gap..self.key
	}

	/// `Previous Delta` for `gap`: 0 for none, 1 for every key below, and otherwise the gap plus
	/// two, which names the start just below it.
	fn previous_delta(&self) -> u64 {
		match self.gap {
			0 => 0,
			gap if gap == self.key => 1,
			gap => gap + 2,
		}
	}

	/// The gap `delta` names below `key`; `None` for one reaching below 0.
	fn gap(key: u64, delta: u64) -> Option<u64> {
		match delta {
			0 => Some(0),
			1 => Some(key),
			delta => Some(delta - 2).filter(|gap| *gap < key),
		}
	}
}

/// The times one Group or Fetch Stream carries: what each FRAME's `Timestamp Delta` counts
/// from, and what GROUP_END's `End` counts from, as raw wire values.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Clock {
	/// The group's start, which GROUP_END counts from on a stream that carried no timestamp.
	start: u64,
	/// The previous FRAME's timestamp, or what the first counts from.
	prev: u64,
	/// The highest timestamp a FRAME on this stream carried, which GROUP_END counts from.
	highest: Option<u64>,
}

impl Clock {
	/// A stream of the group starting at `start`. Lite-07 counts the first FRAME's delta from
	/// that start, so frame 0's is 0; earlier versions count it from 0.
	pub fn new(version: Version, start: u64) -> Self {
		let prev = match version.has_group_end() {
			true => start,
			false => 0,
		};
		Self {
			start,
			prev,
			highest: None,
		}
	}

	/// The `Timestamp Delta` of the next FRAME, at `timestamp`, which it advances to; `None`
	/// when the delta doesn't fit.
	pub fn delta(&mut self, timestamp: u64) -> Option<i64> {
		let delta = (timestamp as i128 - self.prev as i128).try_into().ok()?;
		self.advance(timestamp);
		Some(delta)
	}

	/// The timestamp of the next FRAME, which carries `delta`, and advances to it; `None` when
	/// it falls outside 64 bits.
	pub fn timestamp(&mut self, delta: i64) -> Option<u64> {
		let timestamp = (self.prev as i128 + delta as i128).try_into().ok()?;
		self.advance(timestamp);
		Some(timestamp)
	}

	fn advance(&mut self, timestamp: u64) {
		self.prev = timestamp;
		self.highest = Some(self.highest.map_or(timestamp, |highest| highest.max(timestamp)));
	}

	/// What GROUP_END's `End` counts from: the highest timestamp on the stream, or the group's
	/// start when none was carried, such as on a scale-less track.
	fn base(&self) -> u64 {
		self.highest.unwrap_or(self.start)
	}

	/// GROUP_END's delta for a group ending at `end`; `None` when `end` is below the base, which
	/// a group never is (see [`crate::group::Producer::finish_at`]).
	pub fn end_delta(&self, end: u64) -> Option<u64> {
		end.checked_sub(self.base())
	}

	/// Where a group ends whose GROUP_END carries `delta`; `None` past 64 bits.
	pub fn end(&self, delta: u64) -> Option<u64> {
		self.base().checked_add(delta)
	}
}

impl<'a> Message<'a> for Group {
	/// Every field at its longest varint, so a header still arriving holds few bytes at its
	/// [unidentified](crate::priority::RECV_UNIDENTIFIED) receive rank. Versions before
	/// lite-07 carry fewer fields, in a shorter varint form.
	const MAX_SIZE: usize = Self::FIELDS * crate::coding::varint::MAX_LEN;

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let subscribe = r.varint()?;
		let key = r.varint()?;
		let frame_start = match version.has_frame_bounds() {
			true => r.varint()?,
			false => 0,
		};
		let gap = match version.has_previous() {
			true => Self::gap(key, r.varint()?).ok_or(DecodeError::InvalidValue)?,
			false => 0,
		};

		Ok(Self {
			subscribe,
			key,
			frame_start,
			gap,
		})
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		w.varint(self.subscribe)?;
		w.varint(self.key)?;
		if version.has_frame_bounds() {
			w.varint(self.frame_start)?;
		} else if self.frame_start != 0 {
			// The peer would number the frames from 0 and silently misalign the group.
			return Err(EncodeError::Version);
		}
		// An older peer learns of a gap another way, such as a drop, so the header drops it.
		if version.has_previous() {
			debug_assert!(self.gap <= self.key, "a gap below key 0");
			w.varint(self.previous_delta())?;
		}

		Ok(())
	}
}

#[cfg(test)]
mod test {
	use super::*;

	/// A partial group is self-describing: the stream says which frame it starts at.
	#[test]
	fn frame_start_roundtrips() {
		let msg = Group {
			subscribe: 1,
			key: 7,
			frame_start: 4,
			gap: 0,
		};
		for version in [Version::Lite06, Version::Lite07] {
			let mut buf = Vec::new();
			msg.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)
				.unwrap();
			let got = crate::coding::decode_buf(&mut buf.as_slice(), version, |r, v| Group::decode_msg(r, v)).unwrap();
			assert_eq!((got.key, got.frame_start), (7, 4), "{version:?}");
		}
	}

	/// An older peer would number the frames from 0 and silently misalign the group.
	#[test]
	fn frame_start_rejected_before_lite06() {
		let msg = Group {
			subscribe: 1,
			key: 7,
			frame_start: 4,
			gap: 0,
		};
		let mut buf = Vec::new();
		assert!(
			msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05)
				.is_err()
		);

		let whole = Group { frame_start: 0, ..msg };
		let mut buf = Vec::new();
		whole
			.encode_msg(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05)
			.unwrap();
		let got =
			crate::coding::decode_buf(&mut buf.as_slice(), Version::Lite05, |r, v| Group::decode_msg(r, v)).unwrap();
		assert_eq!(got.frame_start, 0);
	}

	/// The size cap admits the longest header every version can carry: each field at its
	/// form's largest value, whose minimal encoding is already that form's longest. Lite-07
	/// fills the cap exactly, and one byte more is refused at the length prefix.
	#[test]
	fn the_longest_header_reads_back_under_the_cap() {
		let versions = [
			Version::Lite01,
			Version::Lite02,
			Version::Lite03,
			Version::Lite04,
			Version::Lite05,
			Version::Lite06,
			Version::Lite07,
		];
		for version in versions {
			let (max, longest) = match Form::from(version) {
				Form::Quic => (crate::coding::varint::MAX_QUIC, 8),
				Form::LeadingOnes => (u64::MAX, crate::coding::varint::MAX_LEN),
			};
			let fields = 2 + usize::from(version.has_frame_bounds()) + usize::from(version.has_previous());
			// The longest Previous Delta names a gap of all but the lowest key.
			let msg = Group {
				subscribe: max,
				key: max,
				frame_start: if version.has_frame_bounds() { max } else { 0 },
				gap: if version.has_previous() { max - 2 } else { 0 },
			};

			let mut body = Vec::new();
			msg.encode_msg(&mut Encoder::new(&mut body, version.into()), version)
				.unwrap();
			assert_eq!(body.len(), fields * longest, "{version:?}");
			assert!(body.len() <= <Group as Message>::MAX_SIZE, "{version:?}");

			let mut wire = Vec::new();
			msg.encode(&mut Encoder::new(&mut wire, version.into()), version)
				.unwrap();
			let (got, used) = Group::decode_slice(&wire, version).unwrap();
			assert_eq!(used, wire.len(), "{version:?}");
			assert_eq!(
				(got.subscribe, got.key, got.frame_start, got.gap),
				(msg.subscribe, msg.key, msg.frame_start, msg.gap),
				"{version:?}"
			);
		}

		assert_eq!(<Group as Message>::MAX_SIZE, 36);
		let over = <Group as Message>::MAX_SIZE + 1;
		let mut wire = vec![over as u8];
		wire.resize(1 + over, 0);
		assert!(matches!(
			Group::decode_slice(&wire, Version::Lite07),
			Err(DecodeError::MessageTooLarge { size: 37, max: 36 })
		));
	}

	/// `Previous Delta` names no gap with 0, every key below with 1, and otherwise the start
	/// just below the gap, which never reaches below 0.
	#[test]
	fn previous_delta_names_the_gap_below() {
		let header = |key, gap| Group {
			subscribe: 1,
			key,
			frame_start: 0,
			gap,
		};
		let delta = |key, gap| {
			let mut buf = Vec::new();
			header(key, gap)
				.encode_msg(&mut Encoder::new(&mut buf, Version::Lite07.into()), Version::Lite07)
				.unwrap();
			let got = crate::coding::decode_buf(&mut buf.as_slice(), Version::Lite07, |r, v| Group::decode_msg(r, v))
				.unwrap();
			assert_eq!(got.empty_below(), key - gap..key);
			*buf.last().unwrap()
		};
		assert_eq!(delta(10, 0), 0);
		assert_eq!(delta(10, 10), 1, "the first group");
		assert_eq!(delta(0, 0), 0);
		// Group 3 just below: 3 + (6 + 1) = 10.
		assert_eq!(delta(10, 6), 8);
		assert_eq!(delta(10, 9), 11, "group 0 just below");

		// A delta of 2 names the key just below, which holds no gap.
		let decode = |delta: u8| {
			crate::coding::decode_buf(&mut [1, 10, 0, delta].as_slice(), Version::Lite07, |r, v| {
				Group::decode_msg(r, v)
			})
		};
		assert_eq!(decode(2).unwrap().gap, 0);
		assert_eq!(decode(11).unwrap().gap, 9);
		assert!(matches!(decode(12), Err(DecodeError::InvalidValue)), "below group 0");

		// Lite-06 carries no Previous Delta.
		let mut buf = Vec::new();
		header(10, 4)
			.encode_msg(&mut Encoder::new(&mut buf, Version::Lite06.into()), Version::Lite06)
			.unwrap();
		assert_eq!(buf, [1, 10, 0]);
	}
}
