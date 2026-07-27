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
}

impl Message for Group {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let subscribe = u64::decode(r, version)?;
		let sequence = u64::decode(r, version)?;
		let frame_start = match version.has_frame_bounds() {
			true => u64::decode(r, version)?,
			false => 0,
		};

		Ok(Self {
			subscribe,
			sequence,
			frame_start,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.subscribe.encode(w, version)?;
		self.sequence.encode(w, version)?;

		if version.has_frame_bounds() {
			self.frame_start.encode(w, version)?;
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
			sequence: 7,
			frame_start: 4,
		};
		let mut buf = Vec::new();
		msg.encode_msg(&mut buf, Version::Lite06Wip).unwrap();
		let got = Group::decode_msg(&mut buf.as_slice(), Version::Lite06Wip).unwrap();
		assert_eq!((got.sequence, got.frame_start), (7, 4));
	}

	/// An older peer would number the frames from 0 and silently misalign the group.
	#[test]
	fn frame_start_rejected_before_lite06() {
		let msg = Group {
			subscribe: 1,
			sequence: 7,
			frame_start: 4,
		};
		let mut buf = Vec::new();
		assert!(msg.encode_msg(&mut buf, Version::Lite05).is_err());

		let whole = Group { frame_start: 0, ..msg };
		let mut buf = Vec::new();
		whole.encode_msg(&mut buf, Version::Lite05).unwrap();
		let got = Group::decode_msg(&mut buf.as_slice(), Version::Lite05).unwrap();
		assert_eq!(got.frame_start, 0);
	}
}
