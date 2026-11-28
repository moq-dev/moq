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
}

impl Group {
	/// The varints the newest layout carries: Subscribe ID, Group Start and Frame Start.
	const FIELDS: usize = 3;
}

/// The times one Group or Fetch Stream carries: what each FRAME's `Timestamp Delta` counts
/// from, and what GROUP_END's `End` counts from, as raw wire values.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Clock {
	/// The previous FRAME's timestamp, or what the first counts from. On lite-07 a group's
	/// timestamps never decrease, so this is also what GROUP_END counts from.
	prev: u64,
	/// Whether deltas are unsigned (lite-07) rather than zigzag-encoded signed values.
	unsigned: bool,
	/// Whether the next FRAME is lite-07's frame 0, which carries no delta: a group starts at
	/// its first frame's timestamp.
	implicit: bool,
}

impl Clock {
	/// A stream of the group starting at `start`, from frame `frame_start`. Lite-07 omits frame
	/// 0's delta, since it sits at the group's start, and counts every other delta unsigned from
	/// the frame before, or from the start for a stream's first; earlier versions zigzag-encode
	/// signed deltas from 0.
	pub fn new(version: Version, start: u64, frame_start: u64) -> Self {
		let unsigned = version.has_group_end();
		let prev = match unsigned {
			true => start,
			false => 0,
		};
		Self {
			prev,
			unsigned,
			implicit: unsigned && frame_start == 0,
		}
	}

	/// The wire `Timestamp Delta` of the next FRAME, at `timestamp`, advancing the clock: `None`
	/// for lite-07's frame 0, which carries none.
	///
	/// [`Error::TimestampMismatch`] when it doesn't fit: frame 0 away from its group's start, or a
	/// later frame below the one before on lite-07, which a group never holds (see
	/// [`crate::group::Producer::write_frame`]); [`Error::BoundsExceeded`] outside `i64` before
	/// lite-07.
	pub fn encode_delta(&mut self, timestamp: u64) -> Result<Option<u64>, crate::Error> {
		let delta = match (self.implicit, self.unsigned) {
			(true, _) if timestamp != self.prev => return Err(crate::Error::TimestampMismatch),
			(true, _) => None,
			(false, true) => Some(
				timestamp
					.checked_sub(self.prev)
					.ok_or(crate::Error::TimestampMismatch)?,
			),
			(false, false) => {
				let signed = (timestamp as i128 - self.prev as i128)
					.try_into()
					.map_err(|_| crate::Error::BoundsExceeded(BoundsExceeded))?;
				Some(crate::coding::varint::zigzag(signed))
			}
		};
		self.prev = timestamp;
		self.implicit = false;
		Ok(delta)
	}

	/// Whether the next FRAME carries no `Timestamp Delta`: lite-07's frame 0. Its timestamp is
	/// then [`Self::decode_implicit`].
	pub fn implicit(&self) -> bool {
		self.implicit
	}

	/// The timestamp of lite-07's frame 0, its group's start, advancing the clock.
	pub fn decode_implicit(&mut self) -> u64 {
		self.implicit = false;
		self.prev
	}

	/// The timestamp of the next FRAME, which carries wire `delta`, advancing the clock; `None`
	/// when it falls outside 64 bits.
	pub fn decode_delta(&mut self, delta: u64) -> Option<u64> {
		let timestamp = match self.unsigned {
			true => self.prev.checked_add(delta)?,
			false => {
				let delta = crate::coding::varint::unzigzag(delta);
				(self.prev as i128 + delta as i128).try_into().ok()?
			}
		};
		self.prev = timestamp;
		self.implicit = false;
		Some(timestamp)
	}

	/// GROUP_END's delta for a group ending at `end`; `None` when `end` is below the last
	/// timestamp, which a group never is (see [`crate::group::Producer::finish_at`]).
	pub fn end_delta(&self, end: u64) -> Option<u64> {
		end.checked_sub(self.prev)
	}

	/// Where a group ends whose GROUP_END carries `delta`; `None` past 64 bits.
	pub fn end(&self, delta: u64) -> Option<u64> {
		self.prev.checked_add(delta)
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

		Ok(Self {
			subscribe,
			key,
			frame_start,
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
			let fields = 2 + usize::from(version.has_frame_bounds());
			let msg = Group {
				subscribe: max,
				key: max,
				frame_start: if version.has_frame_bounds() { max } else { 0 },
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
				(got.subscribe, got.key, got.frame_start),
				(msg.subscribe, msg.key, msg.frame_start),
				"{version:?}"
			);
		}

		assert_eq!(<Group as Message>::MAX_SIZE, 27);
		let over = <Group as Message>::MAX_SIZE + 1;
		let mut wire = vec![over as u8];
		wire.resize(1 + over, 0);
		assert!(matches!(
			Group::decode_slice(&wire, Version::Lite07),
			Err(DecodeError::MessageTooLarge { size: 28, max: 27 })
		));
	}
}
