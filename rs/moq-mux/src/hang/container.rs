use std::task::Poll;

use bytes::Buf;

use crate::container::Container;
use crate::frame::Frame;

/// hang Legacy format: VarInt timestamp prefix + raw codec bitstream.
pub struct Legacy;

impl Container for Legacy {
	type Error = hang::Error;

	fn write(&self, group: &mut moq_lite::GroupProducer, frame: &Frame) -> Result<(), Self::Error> {
		let hang_frame = hang::container::Frame {
			timestamp: frame.timestamp,
			payload: frame.payload.clone().into(),
		};
		hang_frame.encode(group)
	}

	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Frame>, Self::Error>> {
		use std::task::ready;

		let Some(data) = ready!(group.poll_read_frame(waiter).map_err(hang::Error::from)?) else {
			return Poll::Ready(Ok(None));
		};

		let mut hang_frame = hang::container::Frame::decode(data)?;
		let payload = hang_frame.payload.copy_to_bytes(hang_frame.payload.remaining());

		Poll::Ready(Ok(Some(Frame {
			timestamp: hang_frame.timestamp,
			payload,
		})))
	}
}
