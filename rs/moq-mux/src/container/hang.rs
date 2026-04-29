use std::task::Poll;

use bytes::Buf;

use crate::container::{Cmaf, Container, Frame};

/// Container for the hang protocol.
///
/// `Hang::Legacy` is the original wire format: VarInt timestamp prefix + raw codec bitstream,
/// one media frame per moq-lite frame.
///
/// `Hang::Cmaf` carries CMAF moof+mdat fragments. The contained [`Cmaf`] is parsed once
/// upfront from the catalog's init segment.
pub enum Hang {
	Legacy,
	Cmaf(Cmaf),
}

impl TryFrom<&hang::catalog::Container> for Hang {
	type Error = crate::Error;

	fn try_from(container: &hang::catalog::Container) -> Result<Self, Self::Error> {
		use base64::Engine;

		match container {
			hang::catalog::Container::Legacy => Ok(Self::Legacy),
			hang::catalog::Container::Cmaf { init_data } => {
				let init_bytes = base64::engine::general_purpose::STANDARD
					.decode(init_data)
					.map_err(|e| super::CmafError::Mp4(mp4_atom::Error::Io(std::io::Error::other(e))))?;
				Ok(Self::Cmaf(Cmaf::from_init(&init_bytes)?))
			}
		}
	}
}

impl Container for Hang {
	type Error = crate::Error;

	fn write(&self, group: &mut moq_lite::GroupProducer, frames: &[Frame]) -> Result<(), Self::Error> {
		match self {
			Self::Legacy => {
				for frame in frames {
					let hang_frame = hang::container::Frame {
						timestamp: frame.timestamp,
						payload: frame.payload.clone().into(),
					};
					hang_frame.encode(group)?;
				}
				Ok(())
			}
			Self::Cmaf(cmaf) => cmaf.write(group, frames).map_err(Into::into),
		}
	}

	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Vec<Frame>>, Self::Error>> {
		match self {
			Self::Legacy => {
				use std::task::ready;

				let Some(data) = ready!(group.poll_read_frame(waiter).map_err(hang::Error::from)?) else {
					return Poll::Ready(Ok(None));
				};

				let mut hang_frame = hang::container::Frame::decode(data)?;
				let payload = hang_frame.payload.copy_to_bytes(hang_frame.payload.remaining());

				Poll::Ready(Ok(Some(vec![Frame {
					timestamp: hang_frame.timestamp,
					payload,
					// Legacy can't determine from data; consumer infers from group position.
					keyframe: false,
				}])))
			}
			Self::Cmaf(cmaf) => cmaf.poll_read(group, waiter).map(|r| r.map_err(Into::into)),
		}
	}
}
