//! The original hang wire format.
//!
//! Each moq frame holds one media frame: a VarInt-encoded timestamp
//! followed by the raw codec bitstream. Simple but ad-hoc; new
//! broadcasts should use [`crate::container::loc`] instead.

use std::task::Poll;

use bytes::Buf;

use crate::container::{Container, Frame as MediaFrame};

/// Hang Legacy wire format. Stateless; one instance serves every track.
#[derive(Default)]
pub struct Frame;

impl Frame {
	pub fn new() -> Self {
		Self
	}
}

impl Container for Frame {
	type Error = crate::Error;

	fn write(&self, group: &mut moq_net::GroupProducer, frames: &[MediaFrame]) -> Result<(), Self::Error> {
		for frame in frames {
			let hang_frame = hang::container::Frame {
				timestamp: frame.timestamp,
				payload: frame.payload.clone(),
			};
			hang_frame.encode(group)?;
		}
		Ok(())
	}

	fn poll_read(
		&self,
		group: &mut moq_net::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Vec<MediaFrame>>, Self::Error>> {
		use std::task::ready;

		let Some(data) = ready!(group.poll_read_frame(waiter).map_err(hang::Error::from)?) else {
			return Poll::Ready(Ok(None));
		};

		let mut hang_frame = hang::container::Frame::decode(data)?;
		let payload = hang_frame.payload.copy_to_bytes(hang_frame.payload.remaining());

		Poll::Ready(Ok(Some(vec![MediaFrame {
			timestamp: hang_frame.timestamp,
			payload,
			// Legacy can't determine keyframe from data; the wrapping
			// Consumer infers it from group position instead.
			keyframe: false,
		}])))
	}
}
