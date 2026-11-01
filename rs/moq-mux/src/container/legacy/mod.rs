//! The original hang wire format.
//!
//! Each moq frame holds one media frame: a varint-encoded timestamp
//! followed by the raw codec bitstream. Simple but ad-hoc; new
//! broadcasts should use [`crate::container::loc`] instead.

use crate::container::{Container, Frame, Kind};

/// Hang Legacy wire format configured for the track's media role.
pub struct Wire(
	/// The kind of content carried by the track.
	pub Kind,
);

impl Wire {
	/// The bytes preceding a frame's payload: its varint timestamp.
	pub(crate) fn prefix(&self, timestamp: moq_net::Timestamp) -> crate::Result<bytes::Bytes> {
		let mut prefix = bytes::BytesMut::new();
		hang::container::Frame {
			timestamp,
			payload: bytes::Bytes::new(),
		}
		.encode(&mut prefix)?;
		Ok(prefix.freeze())
	}
}

impl Container for Wire {
	type Error = crate::Error;

	fn end(&self, frame: &Frame) -> Option<moq_net::Timestamp> {
		(self.0 != Kind::Data && frame.payload.is_empty()).then_some(frame.timestamp)
	}

	fn kind(&self) -> Kind {
		self.0
	}

	fn finish_group(
		&self,
		group: &mut moq_net::group::Producer,
		end: Option<moq_net::Timestamp>,
	) -> Result<(), Self::Error> {
		if self.0 == Kind::Video
			&& let Some(timestamp) = end
		{
			hang::container::Frame {
				timestamp,
				payload: bytes::Bytes::new(),
			}
			.write_to(group)?;
		}
		Ok(())
	}

	fn write(&self, group: &mut moq_net::group::Producer, frames: &[Frame]) -> Result<(), Self::Error> {
		for frame in frames {
			let hang_frame = hang::container::Frame {
				timestamp: frame.timestamp,
				payload: frame.payload.clone(),
			};
			hang_frame.write_to(group)?;
		}
		Ok(())
	}

	fn decode(&self, frame: moq_net::frame::Frame) -> Result<Vec<Frame>, Self::Error> {
		// On a data track an object with no bytes is its discontinuity marker, not a frame.
		// Elsewhere it is malformed, so it fails to decode below.
		if self.0 == Kind::Data && frame.payload.is_empty() {
			return Ok(Vec::new());
		}

		let hang_frame = hang::container::Frame::decode(frame.payload)?;
		Ok(vec![Frame {
			timestamp: hang_frame.timestamp,
			payload: hang_frame.payload,
			// Legacy doesn't carry the keyframe bit on the wire; the
			// wrapping Consumer fills it in from group position.
			keyframe: false,
			// Legacy carries no per-frame duration.
			duration: None,
		}])
	}
}
