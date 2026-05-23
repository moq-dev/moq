//! Original hang wire format.
//!
//! Each frame is a VarInt timestamp followed by the raw codec bitstream.
//! Stateless — one [`Legacy`] handles every track.

use std::task::Poll;

use bytes::Buf;

use crate::container::{Container, Frame};

/// Hang Legacy wire format: VarInt timestamp + raw codec bitstream.
#[derive(Default)]
pub struct Legacy;

impl Legacy {
	pub fn new() -> Self {
		Self
	}
}

impl Container for Legacy {
	type Error = crate::Error;

	fn write(&self, group: &mut moq_net::GroupProducer, frames: &[Frame]) -> Result<(), Self::Error> {
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
	) -> Poll<Result<Option<Vec<Frame>>, Self::Error>> {
		use std::task::ready;

		let Some(data) = ready!(group.poll_read_frame(waiter).map_err(hang::Error::from)?) else {
			return Poll::Ready(Ok(None));
		};

		let mut hang_frame = hang::container::Frame::decode(data)?;
		let payload = hang_frame.payload.copy_to_bytes(hang_frame.payload.remaining());

		Poll::Ready(Ok(Some(vec![Frame {
			timestamp: hang_frame.timestamp,
			payload,
			// Legacy can't determine keyframe from data; the wrapping Consumer
			// infers it from group position.
			keyframe: false,
		}])))
	}
}
