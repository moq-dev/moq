use crate::coding::*;

use super::{Message, Version};

#[derive(Clone, Debug)]
pub struct Group {
	// The subscribe ID.
	pub subscribe: u64,

	// The group sequence number
	pub sequence: u64,

	// The index of the first frame on this stream. 0 (the common case) means the stream
	// carries the group from its beginning; a higher value means the leading frames are
	// missing, either because the subscription started partway in or because the
	// publisher only holds the tail. Lite06+ only; older versions always start at 0.
	pub frame_start: u64,

	// The exclusive frame this stream stops at because the subscription ends there, or
	// `None` when it runs to the end of the group. A FIN at it ends only the stream, so a
	// later stream may continue the group. Lite07+ only.
	pub frame_end: Option<u64>,
}

impl Message for Group {
	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let subscribe = r.varint()?;
		let sequence = r.varint()?;
		let frame_start = match version.has_frame_bounds() {
			true => r.varint()?,
			false => 0,
		};
		let frame_end = match version.has_frame_end() {
			true => match r.varint()? {
				0 => None,
				end if end <= frame_start => return Err(DecodeError::InvalidValue),
				end => Some(end),
			},
			false => None,
		};

		Ok(Self {
			subscribe,
			sequence,
			frame_start,
			frame_end,
		})
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		w.varint(self.subscribe)?;
		w.varint(self.sequence)?;

		if version.has_frame_bounds() {
			w.varint(self.frame_start)?;
		} else if self.frame_start != 0 {
			// The peer would number the frames from 0 and silently misalign the group.
			return Err(EncodeError::Version);
		}

		match self.frame_end {
			_ if version.has_frame_end() => w.varint(self.frame_end.unwrap_or(0))?,
			None => {}
			// The peer would read the FIN as the group's end.
			Some(_) => return Err(EncodeError::Version),
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
			sequence: 7,
			frame_start: 4,
			frame_end: None,
		};
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite06.into()), Version::Lite06)
			.unwrap();
		let got = crate::coding::decode_buf(&mut buf.as_slice(), Version::Lite06, Group::decode_msg).unwrap();
		assert_eq!((got.sequence, got.frame_start), (7, 4));
	}

	/// An older peer would number the frames from 0 and silently misalign the group.
	#[test]
	fn frame_start_rejected_before_lite06() {
		let msg = Group {
			subscribe: 1,
			sequence: 7,
			frame_start: 4,
			frame_end: None,
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
		let got = crate::coding::decode_buf(&mut buf.as_slice(), Version::Lite05, Group::decode_msg).unwrap();
		assert_eq!(got.frame_start, 0);
	}

	/// Lite-07 carries where a capped stream stops; lite-06 cannot, so it refuses one
	/// rather than let the FIN read as the group's end. An end at or below the start is
	/// malformed.
	#[test]
	fn frame_end_is_lite07_only() {
		let msg = Group {
			subscribe: 1,
			sequence: 7,
			frame_start: 4,
			frame_end: Some(6),
		};
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite07.into()), Version::Lite07)
			.unwrap();
		let got = crate::coding::decode_buf(&mut buf.as_slice(), Version::Lite07, Group::decode_msg).unwrap();
		assert_eq!((got.frame_start, got.frame_end), (4, Some(6)));

		let mut buf = Vec::new();
		assert!(
			msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite06.into()), Version::Lite06)
				.is_err()
		);

		let mut buf = Vec::new();
		let w = &mut Encoder::new(&mut buf, Version::Lite07.into());
		for value in [1, 7, 4, 4] {
			w.varint(value).unwrap();
		}
		assert!(crate::coding::decode_buf(&mut buf.as_slice(), Version::Lite07, Group::decode_msg).is_err());
	}
}
