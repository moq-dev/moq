use std::task::Poll;

use bytes::Buf;

use crate::container::{Container, Frame};

/// hang Legacy format: VarInt timestamp prefix + raw codec bitstream.
///
/// Each moq-lite frame contains exactly one media frame.
pub struct Legacy;

impl Container for Legacy {
	type Error = hang::Error;

	fn write(&self, group: &mut moq_lite::GroupProducer, frames: &[Frame]) -> Result<(), Self::Error> {
		for frame in frames {
			let hang_frame = hang::container::Frame {
				timestamp: frame.timestamp,
				payload: frame.payload.clone().into(),
			};
			hang_frame.encode(group)?;
		}
		Ok(())
	}

	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
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
			keyframe: false, // Legacy can't determine from data; consumer infers from group position
		}])))
	}
}

#[cfg(feature = "mp4")]
impl Container for hang::catalog::VideoConfig {
	type Error = crate::Error;

	fn write(&self, group: &mut moq_lite::GroupProducer, frames: &[Frame]) -> Result<(), Self::Error> {
		match &self.container {
			hang::catalog::Container::Legacy => Legacy.write(group, frames).map_err(Into::into),
			hang::catalog::Container::Cmaf { timescale, track_id } => {
				crate::cmaf::encode(group, frames, *timescale, *track_id).map_err(Into::into)
			}
		}
	}

	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Vec<Frame>>, Self::Error>> {
		match &self.container {
			hang::catalog::Container::Legacy => Legacy.poll_read(group, waiter).map(|r| r.map_err(Into::into)),
			hang::catalog::Container::Cmaf { timescale, .. } => {
				use std::task::ready;

				let Some(data) = ready!(group.poll_read_frame(waiter)?) else {
					return Poll::Ready(Ok(None));
				};

				Poll::Ready(crate::cmaf::decode(data, *timescale).map(Some).map_err(Into::into))
			}
		}
	}
}

#[cfg(feature = "mp4")]
impl Container for hang::catalog::AudioConfig {
	type Error = crate::Error;

	fn write(&self, group: &mut moq_lite::GroupProducer, frames: &[Frame]) -> Result<(), Self::Error> {
		match &self.container {
			hang::catalog::Container::Legacy => Legacy.write(group, frames).map_err(Into::into),
			hang::catalog::Container::Cmaf { timescale, track_id } => {
				crate::cmaf::encode(group, frames, *timescale, *track_id).map_err(Into::into)
			}
		}
	}

	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Vec<Frame>>, Self::Error>> {
		match &self.container {
			hang::catalog::Container::Legacy => Legacy.poll_read(group, waiter).map(|r| r.map_err(Into::into)),
			hang::catalog::Container::Cmaf { timescale, .. } => {
				use std::task::ready;

				let Some(data) = ready!(group.poll_read_frame(waiter)?) else {
					return Poll::Ready(Ok(None));
				};

				Poll::Ready(crate::cmaf::decode(data, *timescale).map(Some).map_err(Into::into))
			}
		}
	}
}
