use std::task::Poll;

use crate::container::Container;
use crate::frame::{Frame, Timestamp};

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

		let mut buf = data.as_ref();
		let timestamp = Timestamp::decode(&mut buf).map_err(hang::Error::from)?;
		let payload = data.slice((data.len() - buf.len())..);

		Poll::Ready(Ok(Some(Frame { timestamp, payload })))
	}
}
